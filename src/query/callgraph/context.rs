//! One-call symbol context: definition row plus direct caller and callee pages.

use super::frame::{direct_callees, direct_callers};
use super::*;

/// One-call symbol context (Amendment 17): definition line with signature,
/// direct callers, and direct callees — replaces composing callers/callees/
/// symbols for "what does this symbol do and who uses it" questions.
pub fn context(ctx: &QueryCtx<'_>, name: &str, limit: usize) -> Result<LineQuery> {
    let QueryCtx { conn, .. } = *ctx;
    let limit = clamp_limit(limit)?.min(50);
    let (ids, _) = resolve_targets(conn, name, None)?;
    let id = ids[0];

    let def_sql = "SELECT s.file_path, s.line_start, s.line_end, s.kind, s.qualname, s.signature
                   FROM symbols s WHERE s.id = ?1";
    let (def_path, def_line, def_end, kind, qualname, signature): (
        String,
        i64,
        i64,
        String,
        String,
        String,
    ) = conn.query_row(def_sql, [id], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })?;

    let (callers, callers_total) = direct_callers(conn, &[id], limit)?;
    let (callees, callees_total) = direct_callees(conn, &[id], limit)?;

    let mut files = BTreeSet::new();
    files.insert(def_path.clone());
    files.extend(callers.iter().map(|p| p.file.clone()));
    files.extend(callees.iter().map(|p| p.file.clone()));

    let (header, stale) = staleness_header(ctx, &files);
    let total = callers_total + callees_total;
    let shown = callers.len() + callees.len();
    crate::journal::record_result(crate::journal::OutcomeCounts::of(shown, total), stale, 0);

    let mut out = vec![CONTEXT_CAVEAT.to_string()];
    if let Some(header) = header {
        out.push(header);
    }
    let sig = if signature.is_empty() {
        String::new()
    } else {
        format!(" {}", sanitize(&signature))
    };
    out.push(format!(
        "{}:{}-{} {} {}{sig}",
        sanitize(&def_path),
        def_line,
        def_end,
        sanitize(&kind),
        sanitize(&qualname),
    ));

    let last_segment = qualname.split("::").last().unwrap_or("");
    let guards =
        crate::memory::find_guarding_notes(ctx.root, &[&def_path], &[&qualname, last_segment]);
    for g in guards.iter().take(2) {
        out.push(format!(
            "memory: [{}] {}: {}",
            sanitize(&g.kind),
            sanitize(&g.key),
            sanitize(&g.summary)
        ));
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

    Ok(LineQuery::new(out, String::new(), shown))
}
