//! One-call symbol context: definition row plus direct caller and callee pages.

use super::frame::{direct_callees, direct_callers};
use super::*;

struct DefRow {
    path: String,
    line: i64,
    line_end: i64,
    kind: String,
    qualname: String,
    signature: String,
    return_type: String,
}

fn load_definition(conn: &Connection, id: i64) -> Result<DefRow> {
    let def_sql = "SELECT s.file_path, s.line_start, s.line_end, s.kind, s.qualname, s.signature, s.return_type
                   FROM symbols s WHERE s.id = ?1";
    conn.query_row(def_sql, [id], |r| {
        Ok(DefRow {
            path: r.get(0)?,
            line: r.get(1)?,
            line_end: r.get(2)?,
            kind: r.get(3)?,
            qualname: r.get(4)?,
            signature: r.get(5)?,
            return_type: r.get(6)?,
        })
    })
    .map_err(Into::into)
}

/// One-call symbol context (Amendment 17): definition line with signature,
/// direct callers, and direct callees — replaces composing callers/callees/
/// symbols for "what does this symbol do and who uses it" questions.
pub fn context(ctx: &QueryCtx<'_>, name: &str, limit: usize) -> Result<LineQuery> {
    let QueryCtx { conn, .. } = *ctx;
    let limit = clamp_limit(limit)?.min(50);
    let (ids, _) = resolve_targets(ctx, name, None)?;
    let id = ids[0];

    let def = load_definition(conn, id)?;
    let (callers, callers_total) = direct_callers(conn, &[id], limit)?;
    let (callees, callees_total) = direct_callees(conn, &[id], limit)?;

    let mut files = BTreeSet::new();
    files.insert(def.path.clone());
    files.extend(callers.iter().map(|p| p.file.clone()));
    files.extend(callees.iter().map(|p| p.file.clone()));

    let (header, stale) = staleness_header(ctx, &files);
    let total = callers_total + callees_total;
    let shown = callers.len() + callees.len();
    let ambiguous = callers
        .iter()
        .chain(callees.iter())
        .filter(|p| !p.tag.is_empty())
        .count();
    crate::journal::record_result(
        crate::journal::OutcomeCounts::of(shown, total),
        stale,
        ambiguous,
    );

    let out = format_context_lines(
        ctx,
        id,
        &def,
        header,
        limit,
        &callers,
        callers_total,
        &callees,
        callees_total,
    )?;
    let mut lq = LineQuery::new(out, String::new(), shown);
    lq.truncated = shown < total;
    Ok(lq)
}

#[allow(clippy::too_many_arguments)]
fn format_context_lines(
    ctx: &QueryCtx<'_>,
    id: i64,
    def: &DefRow,
    header: Option<String>,
    limit: usize,
    callers: &[Pointer],
    callers_total: usize,
    callees: &[Pointer],
    callees_total: usize,
) -> Result<Vec<String>> {
    let mut out = vec![CONTEXT_CAVEAT.to_string()];
    if let Some(header) = header {
        out.push(header);
    }
    let mut sig = if def.signature.is_empty() {
        String::new()
    } else {
        format!(" {}", sanitize(&def.signature))
    };
    if !def.return_type.is_empty() && !def.signature.contains(&def.return_type) {
        sig.push_str(&format!(" -> {}", sanitize(&def.return_type)));
    }
    out.push(format!(
        "{}:{}-{} {} {}{sig}",
        sanitize(&def.path),
        def.line,
        def.line_end,
        sanitize(&def.kind),
        sanitize(&def.qualname),
    ));

    // Rust qualnames use '.', not '::' — the dot-rsplit works for every
    // language's dotted qualname; the old '::' split returned the whole name
    // for Rust and missed the method for memory-note guards.
    let last_segment = def
        .qualname
        .rsplit_once('.')
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| def.qualname.clone());
    let guards = crate::memory::find_guarding_notes_filtered(
        ctx.root,
        &[&def.path],
        &[&def.qualname, last_segment.as_str()],
        &["decision", "gotcha"],
        2,
    );
    for g in &guards {
        out.push(format!(
            "memory: [{}] {}: {}",
            sanitize(&g.kind),
            sanitize(&g.key),
            sanitize(&g.summary)
        ));
    }

    if matches!(
        SymbolKind::parse(&def.kind),
        Some(SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface)
    ) {
        append_type_details(ctx.conn, id, &def.qualname, limit, &mut out)?;
    }

    out.push(format!(
        "callers (showing {} of {}):",
        callers.len(),
        callers_total
    ));
    out.extend(callers.iter().map(|p| p.render()));
    out.push(format!(
        "callees (showing {} of {}):",
        callees.len(),
        callees_total
    ));
    out.extend(callees.iter().map(|p| p.render()));
    Ok(out)
}

fn append_type_details(
    conn: &Connection,
    id: i64,
    qualname: &str,
    limit: usize,
    out: &mut Vec<String>,
) -> Result<()> {
    let lim = limit as i64;
    let mut bases_stmt = conn.prepare(&format!(
        "SELECT COALESCE(ds.qualname, e.dst_name)
         FROM edges e LEFT JOIN symbols ds ON ds.id = e.dst_id
         WHERE e.src_id = ?1 AND e.kind = '{}'
         ORDER BY 1 LIMIT ?2",
        EdgeKind::Inherits.as_str()
    ))?;
    let bases: Vec<String> = bases_stmt
        .query_map(rusqlite::params![id, lim], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|b| sanitize(&b))
        .collect();
    if !bases.is_empty() {
        out.push(format!("bases: {}", bases.join(", ")));
    }

    let mut impl_stmt = conn.prepare(&format!(
        "SELECT s.qualname
         FROM edges e JOIN symbols s ON s.id = e.src_id
         WHERE e.dst_id = ?1 AND e.kind = '{}'
         ORDER BY s.file_path, s.line_start, s.qualname LIMIT ?2",
        EdgeKind::Inherits.as_str()
    ))?;
    let impls: Vec<String> = impl_stmt
        .query_map(rusqlite::params![id, lim], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|s| sanitize(&s))
        .collect();
    if !impls.is_empty() {
        out.push(format!("implementors: {}", impls.join(", ")));
    }

    let mut fields_stmt = conn.prepare(
        "SELECT field, target_qualname FROM class_fields
         WHERE class_qualname = ?1
         ORDER BY field, target_qualname LIMIT ?2",
    )?;
    let fields: Vec<String> = fields_stmt
        .query_map(rusqlite::params![qualname, lim], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(f, t)| {
            if t.is_empty() {
                sanitize(&f)
            } else {
                format!("{}: {}", sanitize(&f), sanitize(&t))
            }
        })
        .collect();
    if !fields.is_empty() {
        out.push(format!("fields: {}", fields.join(", ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_surfaces_type_details_and_return_type() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL, size INTEGER DEFAULT 0, mtime INTEGER DEFAULT 0, content_hash INTEGER DEFAULT 0);
             CREATE TABLE symbols (id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualname TEXT, file_id INTEGER, file_path TEXT, line_start INTEGER, line_end INTEGER, signature TEXT DEFAULT '', return_type TEXT DEFAULT '');
             CREATE TABLE edges (id INTEGER PRIMARY KEY, src_id INTEGER, dst_id INTEGER, dst_name TEXT, kind TEXT, confidence TEXT, site_line INTEGER);
             CREATE TABLE class_fields (file_id INTEGER, class_qualname TEXT, field TEXT, target_qualname TEXT);
             INSERT INTO files (id, path) VALUES (1, 'a.py');
             INSERT INTO symbols VALUES (1, 'class', 'Base', 'a.Base', 1, 'a.py', 1, 3, 'class Base:', '');
             INSERT INTO symbols VALUES (2, 'class', 'Child', 'a.Child', 1, 'a.py', 5, 10, 'class Child(Base):', '');
             INSERT INTO symbols VALUES (3, 'class', 'Leaf', 'a.Leaf', 1, 'a.py', 12, 15, 'class Leaf(Child):', '');
             INSERT INTO symbols VALUES (4, 'function', 'make', 'a.make', 1, 'a.py', 17, 19, 'def make()', 'a.Child');
             INSERT INTO edges VALUES (1, 2, 1, 'Base', 'inherits', 'resolved', 5);
             INSERT INTO edges VALUES (2, 3, 2, 'Child', 'inherits', 'resolved', 12);
             INSERT INTO class_fields VALUES (1, 'a.Child', 'count', 'int');",
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: std::path::Path::new("."),
        };
        let out = context(&ctx, "a.Child", 10).unwrap().lines.join("\n");
        assert!(out.contains("bases: a.Base"), "{out}");
        assert!(out.contains("implementors: a.Leaf"), "{out}");
        assert!(out.contains("fields: count: int"), "{out}");

        let fn_out = context(&ctx, "a.make", 10).unwrap().lines.join("\n");
        assert!(fn_out.contains("def make() -> a.Child"), "{fn_out}");
    }
}
