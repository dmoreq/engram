//! Callers and callees: direct depth-1 lookups and recursive transitive
//! closures over the resolved call-edge set.

use super::frame::{
    callee_empty_reason, direct_callees, direct_callers, transitive_page, TransitivePlan,
};
use super::*;

/// Shared traversal knobs for `callers`/`callees` (mirrors `ImpactOpts`).
pub struct GraphQueryOpts {
    pub limit: usize,
    pub depth: usize,
}

/// Shared `callers`/`callees` prologue: clamp the knobs and resolve the
/// target name once. Returns the clamped limit/depth and the target symbol ids.
fn prepare_graph_query(
    ctx: &QueryCtx<'_>,
    name: &str,
    opts: GraphQueryOpts,
) -> Result<(usize, usize, Vec<i64>)> {
    let GraphQueryOpts { limit, depth } = opts;
    let limit = clamp_limit(limit)?;
    let depth = depth.clamp(1, crate::query::MAX_DEPTH);
    let (ids, _) = resolve_targets(ctx, name, None)?;
    Ok((limit, depth, ids))
}

pub fn callers(ctx: &QueryCtx<'_>, name: &str, opts: GraphQueryOpts) -> Result<QueryOutput> {
    let QueryCtx { conn, .. } = *ctx;
    let (limit, depth, ids) = prepare_graph_query(ctx, name, opts)?;
    if depth > 1 {
        return callers_transitive(ctx, &ids, name, limit, depth);
    }

    let (callers, total) = direct_callers(conn, &ids, limit)?;
    if total == 0 {
        return Err(EgError::NoResults(callers_empty_reason(conn, &ids, name)));
    }
    collect_output(ctx, callers.into_iter().map(Ok), limit, total)
}

fn callers_empty_reason(conn: &Connection, ids: &[i64], name: &str) -> String {
    let base = "0 callers found (dynamic dispatch possible — grep fallback recommended)";
    if ids.is_empty() {
        return base.to_string();
    }
    let placeholders = crate::db::sql_placeholders(ids.len());
    let params = id_params(ids);
    let type_sql = format!(
        "SELECT 1 FROM symbols WHERE id IN ({placeholders}) AND kind IN ('{}', '{}', '{}') LIMIT 1",
        SymbolKind::Class.as_str(),
        SymbolKind::Trait.as_str(),
        SymbolKind::Interface.as_str(),
    );
    let is_type = conn
        .query_row(&type_sql, rusqlite::params_from_iter(params.iter()), |_| {
            Ok(())
        })
        .is_ok();
    if !is_type {
        return base.to_string();
    }
    let edge_sql = format!(
        "SELECT kind, COUNT(*) FROM edges
         WHERE dst_id IN ({placeholders}) AND kind IN ('{}', '{}', '{}')
         GROUP BY kind",
        EdgeKind::Imports.as_str(),
        EdgeKind::Inherits.as_str(),
        EdgeKind::TypeRef.as_str(),
    );
    let Ok(mut stmt) = conn.prepare(&edge_sql) else {
        return base.to_string();
    };
    let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    }) else {
        return base.to_string();
    };
    let (mut imports, mut inherits, mut type_refs) = (0i64, 0i64, 0i64);
    for row in rows.flatten() {
        match EdgeKind::parse(&row.0) {
            Some(EdgeKind::Imports) => imports = row.1,
            Some(EdgeKind::Inherits) => inherits = row.1,
            Some(EdgeKind::TypeRef) => type_refs = row.1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if imports > 0 {
        parts.push(format!("{imports} imports"));
    }
    if inherits > 0 {
        parts.push(format!("{inherits} inherits"));
    }
    if type_refs > 0 {
        parts.push(format!("{type_refs} type_refs"));
    }
    if parts.is_empty() {
        return base.to_string();
    }
    let s_name = sanitize(name);
    format!(
        "0 callers found (dynamic dispatch possible — grep fallback recommended; referenced by {} — try 'eg impact {s_name} --edges all' or 'eg context {s_name}')",
        parts.join(", ")
    )
}

/// Transitive callers (depth > 1), symmetric with `callees_transitive`.
fn callers_transitive(
    ctx: &QueryCtx<'_>,
    ids: &[i64],
    name: &str,
    limit: usize,
    depth: usize,
) -> Result<QueryOutput> {
    // depth > 1: recursive closure over reverse call edges into the reached
    // set. `reached` keeps one row per caller at its shortest distance, and
    // the NOT EXISTS guard drops callers already reached at or before the
    // destination's distance — so each symbol displays once, at the depth it
    // is first reached, and the seed symbols never reappear as callers.
    let caveat = format!("eg: transitive callers — resolved static call edges only; depth {depth}");
    let (cte, select) = callers_transitive_sql(ids.len());
    transitive_page(
        ctx,
        ids,
        depth,
        limit,
        TransitivePlan {
            sql: (cte, select),
            order_by: "lvl, path, line, qn",
            caveat: Some(caveat),
        },
        || Ok(callers_empty_reason(ctx.conn, ids, name)),
        |r| {
            let line: i64 = r.get(0)?;
            let conf: String = r.get(1)?;
            let path: String = r.get(2)?;
            let qualname: String = r.get(3)?;
            let level: i64 = r.get(4)?;
            Ok(Pointer {
                file: path,
                line,
                tag: confidence_tag(&conf),
                detail: format!("{qualname} [depth={level}]"),
            })
        },
    )
}

/// Recursive closure SQL for transitive callers, mirroring `transitive_sql`
/// in reverse: the first `ids_len` bind parameters are the seed ids, the next
/// two bind the depth cap (closure, then display filter).
fn callers_transitive_sql(ids_len: usize) -> (String, String) {
    let seed = crate::db::sql_placeholders(ids_len);
    let cte = format!(
        "WITH RECURSIVE reach(sym, d) AS (
             SELECT id, 0 FROM symbols WHERE id IN ({seed})
             UNION
             SELECT e.src_id, r.d + 1 FROM edges e JOIN reach r ON e.dst_id = r.sym
             WHERE e.kind = '{}' AND r.d < ?{d}
         ),
         reached(sym, d) AS (SELECT sym, MIN(d) FROM reach GROUP BY sym)",
        EdgeKind::Calls.as_str(),
        d = 1 + ids_len
    );
    let select = format!(
        "SELECT e.site_line AS line, e.confidence AS confidence,
                s.file_path AS path, s.qualname AS qn, r.d + 1 AS lvl
         FROM reached r
         JOIN edges e ON e.dst_id = r.sym
         JOIN symbols s ON s.id = e.src_id
         WHERE e.kind = '{}' AND r.d < ?{d2}
           AND NOT EXISTS (
               SELECT 1 FROM reach r2 WHERE r2.sym = e.src_id AND r2.d <= r.d
           )",
        EdgeKind::Calls.as_str(),
        d2 = 2 + ids_len
    );
    (cte, select)
}

pub fn callees(ctx: &QueryCtx<'_>, name: &str, opts: GraphQueryOpts) -> Result<QueryOutput> {
    let QueryCtx { conn, .. } = *ctx;
    let (limit, depth, ids) = prepare_graph_query(ctx, name, opts)?;
    if depth > 1 {
        return callees_transitive(ctx, &ids, limit, depth);
    }
    let (callees, total) = direct_callees(conn, &ids, limit)?;
    if total == 0 {
        return Err(EgError::NoResults(callee_empty_reason(conn, &ids)?));
    }
    collect_output(ctx, callees.into_iter().map(Ok), limit, total)
}

fn callees_transitive(
    ctx: &QueryCtx<'_>,
    ids: &[i64],
    limit: usize,
    depth: usize,
) -> Result<QueryOutput> {
    // depth > 1: recursive closure over call edges from the reached set.
    let QueryCtx { conn, .. } = *ctx;
    let (cte, select) = transitive_sql(ids.len());
    transitive_page(
        ctx,
        ids,
        depth,
        limit,
        TransitivePlan {
            sql: (cte, select),
            order_by: "lvl, path, line, qn",
            caveat: None,
        },
        || callee_empty_reason(conn, ids),
        |r| {
            let line: i64 = r.get(0)?;
            let conf: String = r.get(1)?;
            let path: String = r.get(2)?;
            let qualname: String = r.get(3)?;
            let level: i64 = r.get(4)?;
            Ok(Pointer {
                file: path,
                line,
                tag: confidence_tag(&conf),
                // Parity with the transitive-callers detail: the hop depth
                // tells the agent how far the callee sits in the graph.
                detail: format!("{qualname} [depth={level}]"),
            })
        },
    )
}

/// Recursive closure SQL for transitive callees. The first `ids_len` bind
/// parameters are the seed ids; the next two bind the depth cap.
fn transitive_sql(ids_len: usize) -> (String, String) {
    let seed = crate::db::sql_placeholders(ids_len);
    let cte = format!(
        "WITH RECURSIVE reach(sym, d) AS (
             SELECT id, 0 FROM symbols WHERE id IN ({seed})
             UNION
             SELECT e.dst_id, r.d + 1 FROM edges e JOIN reach r ON e.src_id = r.sym
             WHERE e.kind = '{}' AND e.dst_id IS NOT NULL AND r.d < ?{d}
         ),
         reached(sym, d) AS (SELECT sym, MIN(d) FROM reach GROUP BY sym)",
        EdgeKind::Calls.as_str(),
        d = 1 + ids_len
    );
    // Numbered placeholders re-reference the seed ids already bound ?1..?N;
    // bare `?`s here would declare new parameters and break the bind count.
    let seed_refs = (1..=ids_len)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let select = format!(
        "SELECT MIN(COALESCE(ds.line_start, e.site_line)) AS line, MAX(e.confidence) AS confidence,
                COALESCE(ds.file_path, s.file_path) AS path, COALESCE(ds.qualname, e.dst_name) AS qn,
                MIN(r.d) + 1 AS lvl
         FROM reached r
         -- CROSS JOIN + WHERE lets SQLite drive from the recursive set and
         -- use edges_src, instead of scanning the whole edge table (R7).
         CROSS JOIN edges e
         JOIN symbols s ON s.id = e.src_id
         LEFT JOIN symbols ds ON ds.id = e.dst_id
         WHERE e.src_id = r.sym AND e.kind = '{}' AND r.d < ?{d2}
           -- A call cycle would otherwise list a seed as its own callee.
           AND COALESCE(ds.id, 0) NOT IN ({seed_refs})
         -- One row per destination: the strongest confidence wins over a
         -- duplicate line row when two edges of different confidence reach
         -- the same symbol. Dst-less edges group by negated edge id to avoid
         -- colliding with positive symbol ids.
         GROUP BY COALESCE(ds.id, -e.id)",
        EdgeKind::Calls.as_str(),
        d2 = 2 + ids_len
    );
    (cte, select)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callees_transitive_does_not_collide_dst_id_with_edge_id() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL, size INTEGER DEFAULT 0, mtime INTEGER DEFAULT 0, content_hash INTEGER DEFAULT 0);
             CREATE TABLE symbols (id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualname TEXT, file_id INTEGER, file_path TEXT, line_start INTEGER, line_end INTEGER);
             CREATE TABLE edges (id INTEGER PRIMARY KEY, src_id INTEGER, dst_id INTEGER, dst_name TEXT, kind TEXT, confidence TEXT, site_line INTEGER);
             CREATE TABLE unresolved_calls (file_id INTEGER, dst_name TEXT, kind TEXT, site_line INTEGER);
             INSERT INTO files (id, path) VALUES (1, 'a.py');
             INSERT INTO symbols VALUES (1, 'function', 'entry', 'a.entry', 1, 'a.py', 1, 5);
             INSERT INTO symbols VALUES (2, 'function', 'helper', 'a.helper', 1, 'a.py', 6, 10);
             INSERT INTO edges VALUES (1, 1, 2, 'helper', 'calls', 'resolved', 2);
             INSERT INTO edges VALUES (2, 1, NULL, 'ext_fn', 'calls', 'resolved', 3);",
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: std::path::Path::new("."),
        };
        let out = callees(
            &ctx,
            "a.entry",
            GraphQueryOpts {
                limit: 10,
                depth: 2,
            },
        )
        .unwrap();
        assert_eq!(
            out.total, 2,
            "resolved dst_id=2 and dst-less edge_id=2 must not collide"
        );
    }

    #[test]
    fn callers_on_type_with_zero_callers_surfaces_non_call_edge_hint() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL, size INTEGER DEFAULT 0, mtime INTEGER DEFAULT 0, content_hash INTEGER DEFAULT 0);
             CREATE TABLE symbols (id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualname TEXT, file_id INTEGER, file_path TEXT, line_start INTEGER, line_end INTEGER);
             CREATE TABLE edges (id INTEGER PRIMARY KEY, src_id INTEGER, dst_id INTEGER, dst_name TEXT, kind TEXT, confidence TEXT, site_line INTEGER);
             INSERT INTO files (id, path) VALUES (1, 'a.py');
             INSERT INTO symbols VALUES (1, 'class', 'Widget', 'a.Widget', 1, 'a.py', 1, 10);
             INSERT INTO symbols VALUES (2, 'module', 'b', 'b', 1, 'a.py', 1, 1);
             INSERT INTO edges VALUES (1, 2, 1, 'Widget', 'imports', 'resolved', 1);
             INSERT INTO edges VALUES (2, 2, 1, 'Widget', 'type_ref', 'resolved', 2);",
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: std::path::Path::new("."),
        };
        let Err(err) = callers(
            &ctx,
            "a.Widget",
            GraphQueryOpts {
                limit: 10,
                depth: 1,
            },
        ) else {
            panic!("expected NoResults");
        };
        let msg = err.to_string();
        assert!(msg.contains("0 callers found"), "{msg}");
        assert!(msg.contains("1 imports, 1 type_refs"), "{msg}");
        assert!(msg.contains("eg impact a.Widget --edges all"), "{msg}");
    }
}
