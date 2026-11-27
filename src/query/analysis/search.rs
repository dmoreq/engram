//! FTS5 symbol search: the exact-identity fast path, the bm25-ranked page,
//! and their deduplicated merge.

use super::*;

/// Cardinality of the exact ∪ FTS result set by symbol identity, computed
/// as `fts_total + exact_total - exact_in_fts` by probing only exact symbol
/// IDs against `symbols_fts`.
fn search_union_total(
    conn: &Connection,
    exact_name: &str,
    fts_expr: &str,
    fts_total: usize,
    exact_total: usize,
) -> Result<usize> {
    if fts_total == 0 {
        return Ok(exact_total);
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT s.id FROM symbols s WHERE {EXACT_IDENTITY_PREDICATE}"
    ))?;
    let exact_ids: Vec<i64> = stmt
        .query_map([exact_name], |r| r.get::<_, i64>(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut exact_in_fts = 0usize;
    crate::db::for_each_chunk(conn, &exact_ids, |conn, placeholders, chunk| {
        let sql = format!(
            "SELECT COUNT(*) FROM symbols_fts
             WHERE symbols_fts MATCH ? AND rowid IN ({placeholders})"
        );
        let mut params = Vec::with_capacity(1 + chunk.len());
        params.push(rusqlite::types::Value::from(fts_expr.to_string()));
        params.extend(id_params(chunk));
        let n: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |r| {
            r.get(0)
        })?;
        exact_in_fts += n as usize;
        Ok(())
    })?;
    Ok(fts_total + exact_total - exact_in_fts)
}

/// Bound the OR expression (R6f Important 7): FTS5 cost is quadratic in the
/// number of prefix terms under OR.
const SEARCH_MAX_TERMS: usize = 32;

/// Build the FTS5 prefix expression with tiered precision:
/// 1) adjacent phrase when qualified (`.`, `/`, `::`),
/// 2) `"t1"* AND "t2"* ...` for multi-token queries,
/// 3) `"t1"* OR "t2"* ...` fallback only when AND yields 0 hits.
fn search_expr(conn: &Connection, terms: &str) -> Result<(Vec<String>, String, Option<String>)> {
    let mut toks: Vec<String> = crate::extract::split_ident(terms)
        .split_whitespace()
        .map(|t| t.to_string())
        .collect();
    let mut terms_cap = None;
    if toks.len() > SEARCH_MAX_TERMS {
        toks.truncate(SEARCH_MAX_TERMS);
        terms_cap = Some(format!("[eg] search terms capped at {SEARCH_MAX_TERMS}"));
    }
    if toks.is_empty() {
        return Err(EgError::Usage("empty search terms".to_string()));
    }
    let quoted: Vec<String> = toks
        .iter()
        .map(|tok| format!("\"{}\"*", tok.replace('"', "")))
        .collect();
    let fts_has_hits = |expr: &str| -> Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH ?1",
            [expr],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };
    if terms.contains('.') || terms.contains('/') || terms.contains("::") {
        let phrase = format!("\"{}\"", toks.join(" ").replace('"', ""));
        if fts_has_hits(&phrase)? {
            return Ok((toks, phrase, terms_cap));
        }
    }
    if toks.len() > 1 {
        let and_expr = quoted.join(" AND ");
        if fts_has_hits(&and_expr)? {
            return Ok((toks, and_expr, terms_cap));
        }
    }
    let expr = quoted.join(" OR ");
    Ok((toks, expr, terms_cap))
}

/// Columns of a search page row, in `search_pointer` order: file, line,
/// kind, qualname, stored one-line signature, callers count. The callers
/// subquery is `db::CALLER_COUNT_SQL` so the "number of callers" definition
/// has one home (search ranking and reuse calibration share it).
fn search_page_columns() -> String {
    format!(
        "s.file_path, s.line_start, s.kind, s.qualname, s.signature, {} AS callers",
        db::CALLER_COUNT_SQL
    )
}

/// Map a search row to the interface card pointer shape:
/// `path:line kind qualname [signature] [callers=N]`.
fn search_pointer(r: &rusqlite::Row<'_>) -> rusqlite::Result<Pointer> {
    let kind: String = r.get(2)?;
    let qualname: String = r.get(3)?;
    let signature: String = r.get(4)?;
    let callers: i64 = r.get(5)?;
    let detail = if signature.is_empty() {
        format!("{kind} {qualname} [callers={callers}]")
    } else {
        format!("{kind} {qualname} {signature} [callers={callers}]")
    };
    Ok(Pointer {
        file: r.get::<_, String>(0)?,
        line: r.get::<_, i64>(1)?,
        tag: "",
        detail,
    })
}

/// FTS page and total.
fn search_fts_page(conn: &Connection, expr: &str, limit: usize) -> Result<(Vec<Pointer>, usize)> {
    let total: usize = conn.query_row(
        "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH ?1",
        [expr],
        |r| r.get::<_, i64>(0),
    )? as usize;
    if total == 0 || limit == 0 {
        return Ok((Vec::new(), total));
    }
    let fetch_limit = (limit * 2).clamp(50, crate::query::MAX_LIMIT);
    let mut stmt = conn.prepare(&format!(
        "SELECT {}, sub.bm25_score
         FROM (
             SELECT ft.rowid AS id, bm25(symbols_fts) AS bm25_score
             FROM symbols_fts ft
             JOIN symbols s ON s.id = ft.rowid
             WHERE symbols_fts MATCH ?1
             ORDER BY bm25(symbols_fts), s.qualname, s.file_path, s.line_start LIMIT ?2
         ) sub
         JOIN symbols s ON s.id = sub.id",
        search_page_columns()
    ))?;
    let mut candidates = stmt
        .query_map(rusqlite::params![expr, fetch_limit as i64], |r| {
            let pointer = search_pointer(r)?;
            let bm25_score: f64 = r.get(6)?;
            let callers: i64 = r.get(5)?;
            let score = bm25_score - 0.3 * (1.0 + callers as f64).ln();
            let path: String = r.get(0)?;
            let line: i64 = r.get(1)?;
            let qualname: String = r.get(3)?;
            Ok((pointer, score, qualname, path, line))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    candidates.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
            .then_with(|| a.4.cmp(&b.4))
    });
    candidates.truncate(limit);
    let ptrs = candidates.into_iter().map(|c| c.0).collect();
    Ok((ptrs, total))
}

/// Exact-identity page and total, empty when the exact set has no rows.
fn search_exact_page(
    conn: &Connection,
    exact_name: &str,
    limit: usize,
) -> Result<(Vec<Pointer>, usize)> {
    let exact_total: usize = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM symbols s
             WHERE {EXACT_IDENTITY_PREDICATE}"
        ),
        [exact_name],
        |r| r.get::<_, i64>(0),
    )? as usize;
    if exact_total == 0 || limit == 0 {
        return Ok((Vec::new(), exact_total));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {}
         FROM symbols s
         WHERE {EXACT_IDENTITY_PREDICATE}
         ORDER BY callers DESC, s.file_path, s.line_start, s.qualname LIMIT ?2",
        search_page_columns(),
    ))?;
    let ptrs = stmt
        .query_map(rusqlite::params![exact_name, limit as i64], search_pointer)?
        .collect::<std::result::Result<_, _>>()?;
    Ok((ptrs, exact_total))
}

/// Exact identities first, then the deduplicated FTS page.
fn merge_search_pointers(exact: Vec<Pointer>, fts: Vec<Pointer>) -> Vec<Pointer> {
    let mut seen: std::collections::HashSet<(String, i64, String)> =
        std::collections::HashSet::new();
    let mut merged = Vec::with_capacity(exact.len() + fts.len());
    for p in exact {
        seen.insert((p.file.clone(), p.line, p.detail.clone()));
        merged.push(p);
    }
    for p in fts {
        if seen.insert((p.file.clone(), p.line, p.detail.clone())) {
            merged.push(p);
        }
    }
    merged
}

/// Symbol-level search (Amendment 12b): FTS5 `bm25()` over split identifier
/// tokens. Hits are symbol identities — the tier field position stays empty
/// (only call-edge pointers carry ` [ambiguous]`). Reuses `collect_output`
/// for cap and post-cap staleness.
pub fn search(ctx: &QueryCtx<'_>, terms: &str, limit: usize) -> Result<LineQuery> {
    let QueryCtx { conn, .. } = *ctx;
    let limit = clamp_limit(limit)?;
    let (_, expr, terms_cap) = search_expr(conn, terms)?;
    let (fts_ptrs, total) = search_fts_page(conn, &expr, limit)?;
    // Exact-identity tier: match the user's original spelling instead of the
    // lowercased token stream — CamelCase and snake_case names only equal
    // `s.name`/`s.qualname` in their original form, whatever the token count.
    let exact_name = terms.trim();
    let (pointers, total) = if exact_name.is_empty() {
        (fts_ptrs, total)
    } else {
        let (exact_ptrs, exact_total) = search_exact_page(conn, exact_name, limit)?;
        if exact_total > 0 {
            // The capped page overlap is not the full overlap: count the
            // union of exact and FTS identities.
            (
                merge_search_pointers(exact_ptrs, fts_ptrs),
                search_union_total(conn, exact_name, &expr, total, exact_total)?,
            )
        } else {
            (fts_ptrs, total)
        }
    };
    if total == 0 {
        return Err(EgError::NoResults(format!(
            "0 symbols matched '{terms}' — try fewer or broader terms"
        )));
    }
    let out = collect_output(
        ctx,
        pointers.into_iter().map(Ok::<_, rusqlite::Error>),
        limit,
        total,
    )?;
    let mut result = search_lines(out)?;
    if let Some(notice) = terms_cap {
        result.stderr = format!("{notice}\n{}", result.stderr);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_expr_prefers_and_then_falls_back_to_or() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE symbols_fts USING fts5(text);
             INSERT INTO symbols_fts(rowid, text) VALUES (1, 'parse json');
             INSERT INTO symbols_fts(rowid, text) VALUES (2, 'parse xml');
             INSERT INTO symbols_fts(rowid, text) VALUES (3, 'format yaml');",
        )
        .unwrap();

        let (_, expr_and, _) = search_expr(&conn, "parse json").unwrap();
        assert_eq!(expr_and, "\"parse\"* AND \"json\"*");

        let (_, expr_or, _) = search_expr(&conn, "parse yaml").unwrap();
        assert_eq!(expr_or, "\"parse\"* OR \"yaml\"*");
    }

    #[test]
    fn search_exact_page_orders_by_callers_desc_before_limit() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE symbols (
                 id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualname TEXT,
                 file_id INTEGER, file_path TEXT, line_start INTEGER,
                 signature TEXT, is_test INTEGER DEFAULT 0
             );
             CREATE TABLE edges (
                 id INTEGER PRIMARY KEY, src_id INTEGER, dst_id INTEGER,
                 dst_name TEXT, kind TEXT, confidence TEXT, site_line INTEGER
             );
             INSERT INTO symbols VALUES (1, 'function', 'run', 'a.run', 1, 'a.py', 1, '', 0);
             INSERT INTO symbols VALUES (2, 'function', 'run', 'z.run', 2, 'z.py', 1, '', 0);
             INSERT INTO symbols VALUES (3, 'function', 'caller1', 'c1', 3, 'c.py', 1, '', 0);
             INSERT INTO symbols VALUES (4, 'function', 'caller2', 'c2', 3, 'c.py', 2, '', 0);
             INSERT INTO edges VALUES (1, 3, 2, 'run', 'calls', 'resolved', 1);
             INSERT INTO edges VALUES (2, 4, 2, 'run', 'calls', 'resolved', 2);",
        )
        .unwrap();

        let (ptrs, total) = search_exact_page(&conn, "run", 1).unwrap();
        assert_eq!(total, 2);
        assert_eq!(ptrs.len(), 1);
        assert_eq!(ptrs[0].file, "z.py");
        assert!(ptrs[0].detail.contains("[callers=2]"), "{}", ptrs[0].detail);
    }
}
