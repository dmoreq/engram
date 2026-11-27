//! Shared traversal frame for the call-graph queries: the direct and
//! transitive count-then-page loaders that both directions reuse.

use super::*;
use crate::extract::EdgeSiteKind;

/// Single-pass windowed fetch shared by `direct_callers` and `direct_callees`:
/// `COUNT(*) OVER ()` computes total rows before `LIMIT`, avoiding a second
/// pass. When `limit == 0`, falls back to `count_sql` so `total` stays exact.
fn single_pass_page(
    conn: &Connection,
    ids: &[i64],
    limit: usize,
    count_sql: &str,
    page_sql: &str,
) -> Result<(Vec<Pointer>, usize)> {
    let mut params = id_params(ids);
    if limit == 0 {
        let total: usize =
            conn.query_row(count_sql, rusqlite::params_from_iter(params.iter()), |r| {
                r.get::<_, i64>(0)
            })? as usize;
        return Ok((Vec::new(), total));
    }
    params.push(rusqlite::types::Value::from(limit as i64));
    let mut stmt = conn.prepare(page_sql)?;
    let mut total: Option<usize> = None;
    let mut pointers = Vec::new();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    while let Some(r) = rows.next()? {
        if total.is_none() {
            total = Some(r.get::<_, i64>(4)? as usize);
        }
        let line = r.get(0)?;
        let conf: String = r.get(1)?;
        let file = r.get(2)?;
        let detail = r.get(3)?;
        pointers.push(Pointer {
            file,
            line,
            tag: confidence_tag(&conf),
            detail,
        });
    }
    Ok((pointers, total.unwrap_or(0)))
}

pub(super) struct TransitivePlan {
    pub(super) sql: (String, String),
    pub(super) order_by: &'static str,
    pub(super) caveat: Option<String>,
}

/// Count-then-bounded-fetch frame shared by the transitive caller/callee
/// traversals (R6 round-2), mirroring `count_then_page` for the recursive
/// closures: the count and the display set bind the seed ids plus the two
/// depth-cap parameters, then the display limit joins as the trailing
/// parameter (`?N`, N = id count + 3).
pub(super) fn transitive_page<E, M>(
    ctx: &QueryCtx<'_>,
    ids: &[i64],
    depth: usize,
    limit: usize,
    plan: TransitivePlan,
    empty_detail: E,
    map: M,
) -> Result<QueryOutput>
where
    E: FnOnce() -> Result<String>,
    M: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Pointer>,
{
    let QueryCtx { conn, .. } = *ctx;
    let (cte, select) = plan.sql;
    let order_by = plan.order_by;
    let caveat = plan.caveat;
    let mut params = id_params(ids);
    params.push(rusqlite::types::Value::from(depth as i64));
    params.push(rusqlite::types::Value::from(depth as i64));
    if limit == 0 {
        let count_sql = format!("{cte} SELECT COUNT(*) FROM ({select})");
        let total: usize =
            conn.query_row(&count_sql, rusqlite::params_from_iter(params.iter()), |r| {
                r.get::<_, i64>(0)
            })? as usize;
        if total == 0 {
            return Err(EgError::NoResults(empty_detail()?));
        }
        let mut out = collect_output(ctx, std::iter::empty(), 0, total)?;
        if let Some(caveat) = caveat {
            prepend_header(&mut out, &caveat);
        }
        return Ok(out);
    }
    // Single recursive CTE execution: COUNT(*) OVER () computes total rows
    // before LIMIT without running the closure twice.
    let sql = format!(
        "{cte} SELECT *, COUNT(*) OVER () AS total_count FROM ({select}) ORDER BY {order_by} LIMIT ?{limit_param}",
        limit_param = ids.len() + 3
    );
    let mut stmt = conn.prepare(&sql)?;
    params.push(rusqlite::types::Value::from(limit as i64));
    let mut total: Option<usize> = None;
    let mut pointers = Vec::new();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    while let Some(r) = rows.next()? {
        if total.is_none() {
            total = Some(r.get::<_, i64>(5)? as usize);
        }
        pointers.push(map(r)?);
    }
    let Some(total) = total else {
        return Err(EgError::NoResults(empty_detail()?));
    };
    let mut out = collect_output(ctx, pointers.into_iter().map(Ok), limit, total)?;
    if let Some(caveat) = caveat {
        prepend_header(&mut out, &caveat);
    }
    Ok(out)
}

/// Side-effect-free direct-caller load. The count and the bounded display set
/// use the same predicate; callers/context render them differently.
pub(super) fn direct_callers(
    conn: &Connection,
    ids: &[i64],
    limit: usize,
) -> Result<(Vec<Pointer>, usize)> {
    let placeholders = crate::db::sql_placeholders(ids.len());
    let count_sql = format!(
        "SELECT COUNT(*) FROM edges e WHERE e.dst_id IN ({placeholders}) AND e.kind = '{}'",
        EdgeKind::Calls.as_str()
    );
    let page_sql = format!(
        "SELECT e.site_line, e.confidence, s.file_path, s.qualname,
                COUNT(*) OVER () AS total_count
         FROM edges e
         JOIN symbols s ON s.id = e.src_id
         WHERE e.dst_id IN ({placeholders}) AND e.kind = '{}'
         ORDER BY s.file_path, e.site_line, s.qualname, s.line_start, e.confidence LIMIT ?{limit_param}",
        EdgeKind::Calls.as_str(),
        limit_param = ids.len() + 1
    );
    single_pass_page(conn, ids, limit, &count_sql, &page_sql)
}

/// Side-effect-free direct-callee load. Mirrors the depth-1 SQL used by
/// `callees`; `context` reuses it with a single id.
pub(super) fn direct_callees(
    conn: &Connection,
    ids: &[i64],
    limit: usize,
) -> Result<(Vec<Pointer>, usize)> {
    let placeholders = crate::db::sql_placeholders(ids.len());
    let count_sql = format!(
        "SELECT COUNT(DISTINCT COALESCE(ds.id, -e.id))
         FROM edges e LEFT JOIN symbols ds ON ds.id = e.dst_id
         WHERE e.src_id IN ({placeholders}) AND e.kind = '{}'",
        EdgeKind::Calls.as_str()
    );
    let page_sql = format!(
        "SELECT MIN(COALESCE(ds.line_start, e.site_line)) AS line,
                MAX(e.confidence) AS confidence,
                MAX(COALESCE(ds.file_path, s.file_path)) AS path,
                COALESCE(ds.qualname, e.dst_name) AS qn,
                COUNT(*) OVER () AS total_count
         FROM edges e
         JOIN symbols s ON s.id = e.src_id
         LEFT JOIN symbols ds ON ds.id = e.dst_id
         WHERE e.src_id IN ({placeholders}) AND e.kind = '{}'
         GROUP BY COALESCE(ds.id, -e.id)
         ORDER BY path, line, qn LIMIT ?{limit_param}",
        EdgeKind::Calls.as_str(),
        limit_param = ids.len() + 1
    );
    single_pass_page(conn, ids, limit, &count_sql, &page_sql)
}

/// Explain an empty `callees` result instead of reporting a resolver gap as
/// absence. Called only when the callee count is 0. Unresolved call sites are
/// attributed to the queried symbol by its file and line range; names are
/// capped so the detail stays one line.
pub(super) fn callee_empty_reason(conn: &Connection, ids: &[i64]) -> Result<String> {
    const SHOWN: usize = 5;
    let names = unresolved_names_in(conn, ids)?;
    if names.is_empty() {
        return Ok("0 callees found".to_string());
    }
    let shown = names
        .iter()
        .take(SHOWN)
        .map(|name| sanitize(name))
        .collect::<Vec<_>>()
        .join(", ");
    let more = if names.len() > SHOWN {
        format!(" (+{} more)", names.len() - SHOWN)
    } else {
        String::new()
    };
    let hint = match symbol_file(conn, ids) {
        Some(path) => format!(" — run eg unresolved --names --path {}", sanitize(&path)),
        None => " — run eg unresolved --names".to_string(),
    };
    Ok(format!(
        "0 callees found (resolved static call edges only); {} unresolved call site(s) inside the queried symbol: {shown}{more}{hint}",
        names.len()
    ))
}

/// Unresolved call names attributed to the queried symbols by file and line range.
fn unresolved_names_in(
    conn: &Connection,
    ids: &[i64],
) -> Result<std::collections::BTreeSet<String>> {
    let mut names = std::collections::BTreeSet::new();
    // One prepare for the whole id set; each id re-executes the statement.
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT u.dst_name
           FROM unresolved_calls u
           JOIN symbols s ON s.file_id = u.file_id
          WHERE s.id = ?1 AND u.kind = '{}'
            AND u.site_line >= s.line_start
            AND u.site_line <= s.line_end
          ORDER BY u.dst_name",
        EdgeSiteKind::Call.as_str()
    ))?;
    for id in ids {
        let rows = stmt.query_map([*id], |r| r.get::<_, String>(0))?;
        for row in rows {
            names.insert(row?);
        }
    }
    Ok(names)
}

fn symbol_file(conn: &Connection, ids: &[i64]) -> Option<String> {
    ids.iter().find_map(|id| {
        conn.query_row(
            "SELECT s.file_path FROM symbols s WHERE s.id = ?1",
            [*id],
            |r| r.get::<_, String>(0),
        )
        .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_pass_direct_callers_and_callees_preserve_totals_and_limit_zero() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE symbols (id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualname TEXT, file_id INTEGER, file_path TEXT, line_start INTEGER, line_end INTEGER);
             CREATE TABLE edges (id INTEGER PRIMARY KEY, src_id INTEGER, dst_id INTEGER, dst_name TEXT, kind TEXT, confidence TEXT, site_line INTEGER);
             INSERT INTO symbols VALUES (1, 'function', 'target', 'm.target', 1, 'a.py', 1, 5);
             INSERT INTO symbols VALUES (2, 'function', 'c1', 'm.c1', 1, 'a.py', 10, 12);
             INSERT INTO symbols VALUES (3, 'function', 'c2', 'm.c2', 1, 'a.py', 20, 22);
             INSERT INTO symbols VALUES (4, 'function', 'd1', 'm.d1', 1, 'a.py', 30, 32);
             INSERT INTO edges VALUES (1, 2, 1, 'target', 'calls', 'resolved', 11);
             INSERT INTO edges VALUES (2, 3, 1, 'target', 'calls', 'resolved', 21);
             INSERT INTO edges VALUES (3, 1, 4, 'd1', 'calls', 'resolved', 2);
             INSERT INTO edges VALUES (4, 1, NULL, 'ext', 'calls', 'resolved', 3);",
        )
        .unwrap();

        let (rows, total) = direct_callers(&conn, &[1], 1).unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].detail, "m.c1");

        let (rows_zero, total_zero) = direct_callers(&conn, &[1], 0).unwrap();
        assert_eq!(total_zero, 2);
        assert!(rows_zero.is_empty());

        let (callees, c_total) = direct_callees(&conn, &[1], 1).unwrap();
        assert_eq!(c_total, 2);
        assert_eq!(callees.len(), 1);
    }
}
