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
    let QueryCtx { conn, root } = *ctx;
    let limit = clamp_limit(limit)?;
    let norm = normalize_repo_path(root, file);
    let rel = indexed_file_opt(conn, &norm)?.ok_or_else(|| EgError::not_indexed(file))?;
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
    ctx: &QueryCtx<'_>,
    name: &str,
    role: Option<&'static str>,
) -> Result<(Vec<i64>, Vec<NameCandidate>)> {
    let (ids, cands) = target_ids(ctx.conn, ctx.root, name)?;
    if ids.is_empty() {
        return Err(EgError::NoResults(symbol_not_found_for(ctx, name)));
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

fn symbol_not_found_for(ctx: &QueryCtx<'_>, name: &str) -> String {
    let trimmed = name.trim();
    if !trimmed.contains('/')
        && !trimmed.contains(' ')
        && ![".py", ".rs", ".ts", ".tsx", ".js", ".jsx"]
            .iter()
            .any(|ext| trimmed.ends_with(ext))
    {
        if let Some((parent, field)) = trimmed.rsplit_once('.') {
            if !parent.is_empty()
                && !field.is_empty()
                && has_indexed_type(ctx.conn, ctx.root, parent)
            {
                return format!(
                    "symbol '{}' not in index (engram indexes callables and types, not field accesses on '{}' — grep for '.{}' instead)",
                    sanitize(name),
                    sanitize(parent),
                    sanitize(field)
                );
            }
        }
    }
    symbol_not_found_msg(name)
}

fn has_indexed_type(conn: &Connection, root: &Path, parent: &str) -> bool {
    let Ok((ids, _)) = target_ids(conn, root, parent) else {
        return false;
    };
    if ids.is_empty() {
        return false;
    }
    let placeholders = crate::db::sql_placeholders(ids.len());
    let sql = format!(
        "SELECT 1 FROM symbols WHERE id IN ({placeholders}) AND kind IN ('{}', '{}', '{}') LIMIT 1",
        SymbolKind::Class.as_str(),
        SymbolKind::Trait.as_str(),
        SymbolKind::Interface.as_str(),
    );
    let params = id_params(&ids);
    conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |_| Ok(()))
        .is_ok()
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
    use super::{symbol_not_found_msg, Connection, Path, QueryCtx};

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

    #[test]
    fn symbol_not_found_emits_field_boundary_hint_for_indexed_class() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL);
             CREATE TABLE symbols (id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualname TEXT, file_id INTEGER, file_path TEXT, line_start INTEGER, line_end INTEGER);
             INSERT INTO files VALUES (1, 'a.py');
             INSERT INTO symbols VALUES (1, 'class', 'Config', 'a.Config', 1, 'a.py', 1, 10);",
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: Path::new("."),
        };
        let msg = super::symbol_not_found_for(&ctx, "Config.timeout");
        assert!(
            msg.contains("callables and types, not field accesses"),
            "{msg}"
        );
        assert!(msg.contains("grep for '.timeout'"), "{msg}");
    }
}
