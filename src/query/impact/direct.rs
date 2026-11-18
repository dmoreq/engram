//! Depth-1 impact phases: grouped per-file rows and per-site rows.

use super::*;

/// Empty default (calls-only) detail: name the import/inherit files the
/// calls-only default skipped, so a file that only has importers points the
/// user at `--edges imports,inherits` instead of a bare zero.
fn importer_caveat_detail(conn: &Connection, rel: &str) -> Result<String> {
    let mut imports = 0usize;
    let mut inherits = 0usize;
    let mut stmt = conn.prepare(
        "SELECT e.kind, COUNT(DISTINCT s.file_path) FROM edges e
         JOIN symbols ds ON ds.id = e.dst_id
         JOIN symbols s ON s.id = e.src_id
         WHERE ds.file_path = ?1 AND e.kind IN ('imports', 'inherits')
         GROUP BY e.kind",
    )?;
    let rows = stmt.query_map([rel], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
    })?;
    for r in rows {
        let (kind, count) = r?;
        if kind == EdgeKind::Imports.as_str() {
            imports = count;
        } else if kind == EdgeKind::Inherits.as_str() {
            inherits = count;
        }
    }
    // Only advise the extra edge kinds when they could actually change the
    // answer; an isolated file gains nothing from a rerun.
    if imports > 0 || inherits > 0 {
        Ok(format!(
            "0 impacting files found (calls only); {imports} file(s) import and {inherits} file(s) inherit from this file — rerun with --edges imports,inherits"
        ))
    } else {
        Ok("0 impacting files found (calls only)".to_string())
    }
}

/// Depth-1 grouped total: distinct source files with any matching edge.
fn impact_grouped_total(conn: &Connection, ctx: &ImpactCtx<'_>) -> Result<usize> {
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT s.file_path) FROM edges e
             {IMPACT_DST_JOIN}
             {EDGE_SRC_JOIN}
             WHERE ds.file_path = ?1 AND e.kind IN ({})",
            ctx.kind_sql
        ),
        [ctx.rel],
        |r| r.get::<_, i64>(0),
    )? as usize)
}

/// Depth-1 grouped rows: one pointer per impacting file with its site count.
fn impact_grouped_rows(conn: &Connection, ctx: &ImpactCtx<'_>) -> Result<Vec<Pointer>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT s.file_path, MIN(e.site_line) AS line,
                COUNT(DISTINCT {}) AS n
         FROM edges e
         {IMPACT_DST_JOIN}
         {EDGE_SRC_JOIN}
         WHERE ds.file_path = ?1 AND e.kind IN ({})
         GROUP BY s.file_path ORDER BY s.file_path LIMIT ?2",
        impact_site_key(),
        ctx.kind_sql
    ))?;
    let rows = stmt.query_map(rusqlite::params![ctx.rel, ctx.limit as i64], |r| {
        Ok(Pointer {
            file: r.get::<_, String>(0)?,
            line: r.get::<_, i64>(1)?,
            tag: "",
            detail: format!("({} sites)", r.get::<_, i64>(2)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Depth-1 grouped rows: one row per impacting file, with its site count.
pub(super) fn impact_direct_grouped(
    qctx: &QueryCtx<'_>,
    ctx: &ImpactCtx<'_>,
) -> Result<QueryOutput> {
    let conn = qctx.conn;
    let total_files = impact_grouped_total(conn, ctx)?;
    if total_files == 0 {
        if ctx.extended {
            return Err(impact_no_results(ctx));
        }
        // Default calls-only mode: a file with importers but no inbound calls
        // is not a bare zero — name the skipped relationships.
        return Err(EgError::NoResultsCaveated {
            caveat: IMPACT_CALLS_CAVEAT,
            detail: importer_caveat_detail(conn, ctx.rel)?,
        });
    }
    let rows = impact_grouped_rows(conn, ctx)?;
    let out = collect_output(qctx, rows.into_iter().map(Ok), ctx.limit, total_files)?;
    Ok(impact_with_caveat(out, ctx.caveat, ctx.extended))
}

/// Depth-1 per-site rows: one row per call site, with optional context/explain.
fn impact_site_total(conn: &Connection, ctx: &ImpactCtx<'_>) -> Result<usize> {
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT {}) FROM edges e
             {IMPACT_DST_JOIN}
             {EDGE_SRC_JOIN}
             WHERE ds.file_path = ?1 AND e.kind IN ({})",
            impact_site_key(),
            ctx.kind_sql
        ),
        [ctx.rel],
        |r| r.get::<_, i64>(0),
    )? as usize)
}

fn impact_site_rows(conn: &Connection, ctx: &ImpactCtx<'_>) -> Result<Vec<Pointer>> {
    let sql = format!(
        "SELECT MIN(e.site_line), MIN(e.confidence), s.file_path
         FROM edges e
         {IMPACT_DST_JOIN}
         {EDGE_SRC_JOIN}
         WHERE ds.file_path = ?1 AND e.kind IN ({})
         GROUP BY {}
         ORDER BY s.file_path, MIN(e.site_line), MIN(s.qualname) LIMIT ?2",
        ctx.kind_sql,
        impact_site_key()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![ctx.rel, ctx.limit as i64], |r| {
        let site: i64 = r.get(0)?;
        let conf: String = r.get(1)?;
        let path: String = r.get(2)?;
        Ok(Pointer {
            file: path,
            line: site,
            tag: confidence_tag(&conf),
            detail: String::new(),
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub(super) fn impact_direct_sites(qctx: &QueryCtx<'_>, ctx: &ImpactCtx<'_>) -> Result<QueryOutput> {
    let conn = qctx.conn;
    let total = impact_site_total(conn, ctx)?;
    if total == 0 {
        return Err(impact_no_results(ctx));
    }
    let rows = impact_site_rows(conn, ctx)?;
    let out = collect_output(qctx, rows.into_iter().map(Ok), ctx.limit, total)?;
    Ok(impact_with_caveat(out, ctx.caveat, ctx.extended))
}
