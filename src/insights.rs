//! P1.5 insight command: `stats`.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::db;
use crate::error::{EgError, Result};
use crate::extract::EdgeKind;
use crate::query::QueryCtx;

/// Counter set shared by the global and scoped `stats` renderings.
struct StatsTotals {
    files: i64,
    symbols: i64,
    edges: i64,
    calls: i64,
    imports: i64,
    inherits: i64,
    perr: i64,
    unresolved: i64,
}

pub fn stats(ctx: &QueryCtx<'_>, path: Option<&str>) -> Result<String> {
    let QueryCtx { conn, root } = *ctx;
    // `--path` scopes every count to one repo-relative prefix; absent keeps
    // the global build-time counters (O(1)).
    let scope = match path {
        Some(p) => {
            crate::query::validate_path_scope(p, false)?;
            Some(crate::query::normalize_scope(p.trim()).to_string())
        }
        None => None,
    };
    let meta = load_meta_map(conn)?;
    let totals = match &scope {
        None => global_totals(conn, &meta)?,
        Some(scope) => scoped_totals(conn, scope)?,
    };
    let size = db::file_size(&db::db_path(root));
    let eg_version = meta
        .get(db::meta_keys::ENGRAM_VERSION)
        .cloned()
        .unwrap_or_else(|| "?".into());
    let grammar = meta
        .get(db::meta_keys::GRAMMAR_VERSION)
        .cloned()
        .unwrap_or_else(|| "?".into());
    Ok(stats_line(
        scope.as_deref(),
        &totals,
        size,
        &eg_version,
        &grammar,
    ))
}

fn load_meta_map(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM meta")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut map = HashMap::new();
    for row in rows {
        let (k, v) = row?;
        map.insert(k, v);
    }
    Ok(map)
}

fn global_totals(conn: &Connection, meta: &HashMap<String, String>) -> Result<StatsTotals> {
    let counter = |key: &str| -> Result<i64> {
        if let Some(val) = meta.get(key).and_then(|v| v.parse::<i64>().ok()) {
            return Ok(val);
        }
        db::meta_count(conn, key)
    };
    Ok(StatsTotals {
        files: counter(db::meta_keys::FILE_COUNT)?,
        symbols: counter(db::meta_keys::SYMBOL_COUNT)?,
        edges: counter(db::meta_keys::EDGE_COUNT)?,
        calls: counter(db::meta_keys::EDGE_CALLS)?,
        imports: counter(db::meta_keys::EDGE_IMPORTS)?,
        inherits: counter(db::meta_keys::EDGE_INHERITS)?,
        perr: counter(db::meta_keys::PARSE_ERROR_COUNT)?,
        // Meta only: this read path has no COUNT fallback (pre-counter
        // indexes report 0 here, as before).
        unresolved: meta
            .get(db::meta_keys::UNRESOLVED_CALLEE_COUNT)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
    })
}

/// Scoped totals: one range scan per table. Edges attribute to the source
/// symbol's file, matching `report`; the three edge kinds come out of the
/// same conditional-SUM scan as the build-time `meta_counts` (one query, not
/// one per kind).
fn scoped_totals(conn: &Connection, scope: &str) -> Result<StatsTotals> {
    let (clause, params) = crate::query::scoped_params("f", "path", Some(scope), 1);
    let (files, perr, unresolved) = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(f.parse_status != 'ok'), 0), COALESCE(SUM(f.unresolved), 0)
             FROM files f WHERE 1=1 {clause}"
        ),
        rusqlite::params_from_iter(params.iter()),
        crate::db::ints3,
    )?;
    if files == 0 {
        return Err(EgError::NoResults(format!("no indexed files in '{scope}'")));
    }
    let edge_from = format!("edges e {}", db::EDGE_SRC_FILE_JOIN);
    let (edges, calls, imports, inherits) = conn.query_row(
        &format!(
            "SELECT COUNT(*),
            COALESCE(SUM(e.kind = '{}'), 0),
            COALESCE(SUM(e.kind = '{}'), 0),
            COALESCE(SUM(e.kind = '{}'), 0)
     FROM {edge_from} WHERE 1=1 {clause}",
            EdgeKind::Calls.as_str(),
            EdgeKind::Imports.as_str(),
            EdgeKind::Inherits.as_str(),
        ),
        rusqlite::params_from_iter(params.iter()),
        crate::db::ints4,
    )?;
    let symbols: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM symbols s JOIN files f ON f.id = s.file_id WHERE 1=1 {clause}"
        ),
        rusqlite::params_from_iter(params.iter()),
        |r| r.get(0),
    )?;
    Ok(StatsTotals {
        files,
        symbols,
        edges,
        calls,
        imports,
        inherits,
        perr,
        unresolved,
    })
}

fn stats_line(
    scope: Option<&str>,
    totals: &StatsTotals,
    size: u64,
    eg_version: &str,
    grammar: &str,
) -> String {
    let scope_prefix = scope.map(|s| format!("scope={s} ")).unwrap_or_default();
    let StatsTotals {
        files,
        symbols,
        edges,
        calls,
        imports,
        inherits,
        perr,
        unresolved,
    } = totals;
    format!(
        "eg stats: {scope_prefix}files={files} symbols={symbols} edges={edges} (calls={calls} imports={imports} inherits={inherits}) parse_errors={perr} unresolved={unresolved} db_bytes={size} eg={eg_version} grammar={grammar}"
    )
}
