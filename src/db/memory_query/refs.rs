use super::*;

/// Candidates from code-anchored `refs`: current unexpired rows whose `refs`
/// field matches any query token, window-filtered before the cap.
/// Uses `memories_active_refs` (single scan over active non-empty `refs`) and
/// matches normalized ref tokens against a `HashSet` in Rust.
pub fn memory_ref_candidate_rows(
    conn: &Connection,
    tokens: &[String],
    window: &crate::memory::TimeWindow,
    now: i64,
    cap: usize,
) -> Result<Vec<MemRow>> {
    if tokens.is_empty() || cap == 0 {
        return Ok(Vec::new());
    }
    let token_set: std::collections::HashSet<String> = tokens
        .iter()
        .map(|t| t.trim().trim_start_matches("./").to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if token_set.is_empty() {
        return Ok(Vec::new());
    }
    let tokens_vec: Vec<&str> = token_set.iter().map(String::as_str).collect();
    let slash_tokens: Vec<&str> = tokens_vec
        .iter()
        .copied()
        .filter(|t| t.contains('/'))
        .collect();
    let cols = mem_row_sql(conn);
    let win_exp = window_expiry_clause("", 1, 2, 3);
    let sql = format!(
        "SELECT {cols} FROM memories
         WHERE superseded_by IS NULL AND refs != ''
           AND {win_exp}
         ORDER BY created_at DESC, id DESC"
    );
    let params = [
        rusqlite::types::Value::from(now),
        window.since.unwrap_or(-1).into(),
        window.until.unwrap_or(-1).into(),
    ];
    let mut stmt = conn.prepare(&sql).map_err(map_memory_err)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), mem_row)
        .map_err(map_memory_err)?;
    let mut matched = Vec::with_capacity(cap.min(64));
    for row in rows {
        let row = row.map_err(map_memory_err)?;
        if refs_match_tokens(&row.refs, &token_set, &tokens_vec, &slash_tokens) {
            matched.push(row);
        }
    }
    matched.sort_by(|a, b| b.id.cmp(&a.id));
    matched.truncate(cap);
    Ok(matched)
}

fn strip_line_suffix(r: &str) -> &str {
    let Some((prefix, suffix)) = r.rsplit_once(':') else {
        return r;
    };
    if prefix.ends_with(':') || suffix.is_empty() {
        return r;
    }
    let is_range = suffix.split_once('-').is_some_and(|(a, b)| {
        matches!(
            (a.parse::<u64>(), b.parse::<u64>()),
            (Ok(s), Ok(e)) if s >= 1 && e >= s
        )
    });
    if is_range || suffix.parse::<u64>().is_ok_and(|l| l >= 1) {
        prefix
    } else {
        r
    }
}

fn refs_match_tokens(
    refs: &str,
    token_set: &std::collections::HashSet<String>,
    tokens: &[&str],
    slash_tokens: &[&str],
) -> bool {
    refs.split_whitespace().any(|raw| {
        let norm = raw.trim_start_matches("./").to_ascii_lowercase();
        let base = strip_line_suffix(&norm);
        token_set.contains(&norm)
            || norm.split(['/', ':']).filter(|s| !s.is_empty()).any(|seg| {
                token_set.contains(seg)
                    || seg
                        .split('.')
                        .filter(|s| !s.is_empty())
                        .any(|s| token_set.contains(s))
            })
            || slash_tokens
                .iter()
                .any(|st| base.strip_suffix(st).is_some_and(|p| p.ends_with('/')))
            || tokens.iter().any(|tok| norm.contains(tok))
    })
}
