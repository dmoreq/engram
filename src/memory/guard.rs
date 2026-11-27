//! Ambient memory retrieval: co-retrieve active gotchas/decisions
//! referencing target definitions, files, or symbols.

use std::collections::HashSet;
use std::path::Path;

/// Similarity threshold at which `mem write` suggests consolidating through
/// `--supersedes` instead of creating a near-duplicate note.
pub const CONSOLIDATION_THRESHOLD_PCT: u64 = 80;

/// One near-duplicate note found on the write path.
#[derive(Debug, Clone)]
pub struct ConsolidationMatch {
    pub key: String,
    pub rev: i64,
    pub similarity_pct: u64,
}

impl ConsolidationMatch {
    /// Advisory line appended to `mem write` stdout (CLI and MCP share it).
    pub fn hint_line(&self, new_key: &str) -> String {
        format!(
            "eg: mem hint: '{new_key}' is {}% similar to existing '{}' rev {} — consider --supersedes {}",
            self.similarity_pct, self.key, self.rev, self.key
        )
    }
}

/// English function words that carry no note identity. Removing them keeps a
/// paraphrase above the consolidation threshold without matching unrelated
/// prose on glue words alone.
const CONSOLIDATION_STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "but", "by", "for", "from", "if",
    "in", "into", "is", "it", "its", "no", "not", "of", "on", "or", "over", "than", "that", "the",
    "then", "this", "to", "was", "we", "were", "when", "where", "which", "while", "with",
];

/// Token set used by the consolidation guard: body, refs, and tags through the
/// single memory tokenizer, so the same words that rank a query also drive the
/// duplicate signal.
fn consolidation_tokens(body: &str, refs: &str, tags: &str, legacy: bool) -> HashSet<String> {
    let raw = if legacy {
        crate::memory::mem_tokens_legacy(body)
    } else {
        crate::memory::mem_tokens(body)
    };
    let mut set: HashSet<String> = raw
        .into_iter()
        .filter(|token| !CONSOLIDATION_STOPWORDS.contains(&token.as_str()))
        .collect();
    set.extend(if legacy {
        crate::memory::mem_ref_tag_tokens_legacy(refs, tags)
    } else {
        crate::memory::mem_ref_tag_tokens(refs, tags)
    });
    set
}

/// Jaccard similarity in percent between a candidate note and one stored row.
/// Both sides pass through [`consolidation_tokens`]; 0 when either is empty.
#[cfg(test)]
pub fn consolidation_similarity(
    note: &crate::memory::NormalizedNote,
    row: &crate::db::MemRow,
    legacy: bool,
) -> u64 {
    let left = consolidation_tokens(&note.body, &note.refs, &note.tags, legacy);
    let right = consolidation_tokens(&row.body, &row.refs, &row.tags, legacy);
    tokens_similarity(&left, &right)
}

fn tokens_similarity(left: &HashSet<String>, right: &HashSet<String>) -> u64 {
    if left.is_empty() || right.is_empty() {
        return 0;
    }
    let shared = left.intersection(right).count();
    let union = left.union(right).count();
    (shared * 100).checked_div(union).unwrap_or(0) as u64
}

/// Search the current notes for the closest near-duplicate of a pending
/// write. The same key is excluded (same-key updates use the revision chain)
/// and `--supersedes` already expresses the user's intent, so the caller skips
/// this check in that case.
///
/// Candidate generation uses the memory FTS index (OR terms, top 32) instead
/// of scanning every current note: the guard runs at the `mem write` trust
/// boundary, where tokenizing the whole store would blow the 2 ms budget.
pub fn find_consolidation_match(
    conn: &rusqlite::Connection,
    note: &crate::memory::NormalizedNote,
) -> Option<ConsolidationMatch> {
    let ok_v2 = crate::db::memory_fts_v2_usable(conn).ok().unwrap_or(false);
    let legacy = !ok_v2;
    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    let raw_body = if legacy {
        crate::memory::mem_tokens_legacy(&note.body)
    } else {
        crate::memory::mem_tokens(&note.body)
    };
    let raw_tags = if legacy {
        crate::memory::mem_tokens_legacy(&note.tags)
    } else {
        crate::memory::mem_tokens(&note.tags)
    };
    for token in raw_body.into_iter().chain(raw_tags) {
        if !CONSOLIDATION_STOPWORDS.contains(&token.as_str()) && seen.insert(token.clone()) {
            terms.push(token);
        }
    }
    if terms.is_empty() {
        return None;
    }
    let query = terms.into_iter().take(32).collect::<Vec<_>>().join(" OR ");
    let now = crate::db::unix_now();
    let window = crate::memory::TimeWindow::default();
    let ids: Vec<i64> = crate::db::memory_fts_candidates(conn, &query, &window, now, ok_v2, 32)
        .ok()?
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    if ids.is_empty() {
        return None;
    }
    let filter = crate::db::MemFilter {
        kind: None,
        scope: None,
        now,
        include_superseded: false,
        window,
    };
    let left = consolidation_tokens(&note.body, &note.refs, &note.tags, legacy);
    if left.is_empty() {
        return None;
    }
    crate::db::memory_rows_by_ids(conn, &ids)
        .ok()?
        .into_iter()
        .filter(|row| row.key != note.key && crate::db::memory_row_is_current(row, &filter))
        .filter_map(|row| {
            let right = consolidation_tokens(&row.body, &row.refs, &row.tags, legacy);
            let similarity_pct = tokens_similarity(&left, &right);
            (similarity_pct >= CONSOLIDATION_THRESHOLD_PCT).then_some(ConsolidationMatch {
                key: row.key,
                rev: row.rev,
                similarity_pct,
            })
        })
        .max_by(|a, b| {
            a.similarity_pct
                .cmp(&b.similarity_pct)
                .then_with(|| b.key.cmp(&a.key))
        })
}

/// Compact guarded note info for ambient retrieval.
#[derive(Debug, Clone)]
pub struct GuardedNote {
    pub key: String,
    pub kind: String,
    pub summary: String,
    pub target: String,
}

fn ends_with_delim(haystack: &str, needle: &str) -> bool {
    haystack.strip_suffix(needle).is_some_and(|prefix| {
        if haystack.contains('/') {
            prefix.ends_with('/')
        } else {
            prefix.ends_with("::") || prefix.ends_with('.')
        }
    })
}

/// Check if a note's `refs` matches a target path or symbol.
pub fn ref_matches_target(refs: &str, target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    let target_norm = target.trim_start_matches("./");
    refs.split_whitespace().any(|r| {
        let r_norm = r.trim_start_matches("./");
        r_norm == target_norm
            || ends_with_delim(r_norm, target_norm)
            || ends_with_delim(target_norm, r_norm)
    })
}

/// First line of body, capped safely at 80 characters.
pub fn compact_summary(body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("").trim();
    if first_line.chars().count() > 80 {
        let truncated: String = first_line.chars().take(77).collect();
        format!("{truncated}...")
    } else {
        first_line.to_string()
    }
}

/// Find active gotchas and decisions in local and global memory stores
/// that reference any of the provided paths or symbols.
pub fn find_guarding_notes(root: &Path, paths: &[&str], symbols: &[&str]) -> Vec<GuardedNote> {
    find_guarding_notes_filtered(root, paths, symbols, &["decision", "gotcha"], usize::MAX)
}

/// Find active guarding notes filtered by `kinds` and capped at `limit`.
pub fn find_guarding_notes_filtered(
    root: &Path,
    paths: &[&str],
    symbols: &[&str],
    kinds: &[&str],
    limit: usize,
) -> Vec<GuardedNote> {
    if limit == 0 || kinds.is_empty() || (paths.is_empty() && symbols.is_empty()) {
        return Vec::new();
    }
    let q = GuardQuery(crate::db::unix_now(), paths, symbols, kinds, limit);
    let (mut results, mut seen_keys) = (Vec::new(), HashSet::new());
    if let Ok(local_conn) = crate::db::open_memory_query(root) {
        collect_guards(&local_conn, &q, &mut seen_keys, &mut results);
    }
    if results.len() < limit {
        if let Ok(global_conn) = crate::db::open_global_memory_query() {
            collect_guards(&global_conn, &q, &mut seen_keys, &mut results);
        }
    }
    results
}

struct GuardQuery<'a>(i64, &'a [&'a str], &'a [&'a str], &'a [&'a str], usize);

fn collect_guards(
    conn: &rusqlite::Connection,
    q: &GuardQuery<'_>,
    seen_keys: &mut HashSet<String>,
    out: &mut Vec<GuardedNote>,
) {
    let GuardQuery(now, paths, symbols, kinds, limit) = *q;
    if out.len() >= limit || kinds.is_empty() {
        return;
    }
    let placeholders = crate::db::sql_placeholders(kinds.len());
    let make_sql = |order_by: &str| {
        format!(
            "SELECT id, key, kind, refs FROM memories
             WHERE superseded_by IS NULL AND kind IN ({placeholders})
               AND (expires_at = 0 OR expires_at > ?) AND refs != ''
             ORDER BY {order_by}"
        )
    };
    let Ok(mut stmt) = conn
        .prepare(&make_sql(
            "MAX(anchored_at, created_at) DESC, created_at DESC, key ASC",
        ))
        .or_else(|_| conn.prepare(&make_sql("created_at DESC, key ASC")))
    else {
        return;
    };
    let params: Vec<rusqlite::types::Value> = kinds
        .iter()
        .map(|k| rusqlite::types::Value::from((*k).to_string()))
        .chain(std::iter::once(rusqlite::types::Value::from(now)))
        .collect();
    let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    }) else {
        return;
    };
    let mut matched: Vec<(i64, String, String, String)> = Vec::new();
    for (id, key, kind, refs) in rows.flatten() {
        if out.len() + matched.len() >= limit {
            break;
        }
        if seen_keys.contains(&key) {
            continue;
        }
        if let Some(target) = paths
            .iter()
            .chain(symbols.iter())
            .find(|t| ref_matches_target(&refs, t))
        {
            seen_keys.insert(key.clone());
            matched.push((id, key, kind, target.to_string()));
        }
    }
    if matched.is_empty() {
        return;
    }
    let id_placeholders = crate::db::sql_placeholders(matched.len());
    let id_sql = format!("SELECT id, body FROM memories WHERE id IN ({id_placeholders})");
    let mut bodies = std::collections::HashMap::new();
    if let Ok(mut b_stmt) = conn.prepare(&id_sql) {
        let ids: Vec<rusqlite::types::Value> =
            matched.iter().map(|(id, _, _, _)| (*id).into()).collect();
        if let Ok(b_rows) = b_stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        }) {
            bodies.extend(b_rows.flatten());
        }
    }
    for (id, key, kind, target) in matched {
        let body = bodies.get(&id).map(String::as_str).unwrap_or("");
        out.push(GuardedNote {
            key,
            kind,
            summary: compact_summary(body),
            target,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(body: &str, refs: &str, tags: &str) -> crate::memory::NormalizedNote {
        crate::memory::NormalizedNote {
            key: "new-note".into(),
            kind: crate::memory::MemKind::Fact,
            tags: tags.into(),
            refs: refs.into(),
            scope: crate::memory::MemScope::Durable,
            body: body.into(),
            ttl_secs: 0,
        }
    }

    fn row(key: &str, body: &str, refs: &str, tags: &str) -> crate::db::MemRow {
        crate::db::MemRow {
            id: 1,
            key: key.into(),
            rev: 1,
            kind: "fact".into(),
            scope: "durable".into(),
            session: String::new(),
            body: body.into(),
            tags: tags.into(),
            refs: refs.into(),
            source: "cli".into(),
            created_at: 1,
            anchored_at: 0,
            expires_at: 0,
            superseded_by: None,
        }
    }

    #[test]
    #[ignore = "release gate: cargo test --release --lib consolidation_scan -- --ignored --nocapture"]
    fn consolidation_scan_meets_2ms_on_256_notes() {
        use std::time::{Duration, Instant};
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../schema/memory.sql"))
            .unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..256_i64 {
            let key = format!("note-{i}");
            let body = format!("resolver keeps confidence tiers over resolved edges with invariant {i} and stable behavior");
            let id: i64 = tx.query_row(
                "INSERT INTO memories (key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, expires_at)
                 VALUES (?1, 1, 'fact', 'durable', '', ?2, 'resolver', 'src/query.rs', 0, 'cli', 1, 0) RETURNING id",
                rusqlite::params![key, body], |r| r.get(0),
            ).unwrap();
            tx.execute(
                "INSERT INTO memories_fts(rowid, text) VALUES (?1, ?2)",
                rusqlite::params![id, crate::memory::mem_fts_text(&key, "resolver", &body)],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let candidate = note(
            "the resolver keeps confidence tiers on resolved edges with stable invariants",
            "src/query.rs",
            "resolver",
        );
        let start = Instant::now();
        let found = find_consolidation_match(&conn, &candidate).is_some();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(2),
            "C3 latency gate: {elapsed:?} >= 2ms (match={found})"
        );
    }

    #[test]
    fn identical_notes_score_one_hundred() {
        let n = note(
            "resolver keeps confidence tiers over edges",
            "src/query.rs",
            "resolver",
        );
        let r = row(
            "old",
            "resolver keeps confidence tiers over edges",
            "src/query.rs",
            "resolver",
        );
        assert_eq!(consolidation_similarity(&n, &r, false), 100);
    }

    #[test]
    fn paraphrase_clears_the_threshold_while_unrelated_prose_does_not() {
        let candidate = note(
            "the resolver keeps confidence tiers over resolved edges",
            "src/query/callgraph.rs",
            "resolver",
        );
        let paraphrase = row(
            "old",
            "resolver keeps confidence tiers on resolved edges",
            "src/query/callgraph.rs",
            "resolver",
        );
        assert!(
            consolidation_similarity(&candidate, &paraphrase, false) >= CONSOLIDATION_THRESHOLD_PCT
        );
        let unrelated = row(
            "other",
            "dashboard renders latency sparklines with ANSI colors",
            "src/dashboard.rs",
            "telemetry",
        );
        assert!(consolidation_similarity(&candidate, &unrelated, false) < 20);
    }

    #[test]
    fn synthetic_pair_gate_meets_acceptance_thresholds() {
        // 100 paraphrase pairs (one glue word changed) against 100 unrelated
        // pairs: detection must stay >= 80% and false alarms <= 10%.
        let base = [
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
        ];
        let mut detected = 0;
        let mut false_alarms = 0;
        for i in 0..100 {
            let body = base.join(" ");
            let variant = body.replace(base[i % base.len()], "lambda");
            let candidate = note(&format!("{body} the note"), "", "");
            let stored = row("old", &format!("{variant} a note"), "", "");
            if consolidation_similarity(&candidate, &stored, false) >= CONSOLIDATION_THRESHOLD_PCT {
                detected += 1;
            }
            let unrelated_body = [
                "omega", "sigma", "tau", "upsilon", "phi", "chi", "psi", "rho", "nu", "mu",
            ]
            .join(" ");
            let unrelated = row("other", &unrelated_body, "", "");
            if consolidation_similarity(&candidate, &unrelated, false)
                >= CONSOLIDATION_THRESHOLD_PCT
            {
                false_alarms += 1;
            }
        }
        assert!(detected >= 80, "detection rate {detected}% below gate");
        assert!(
            false_alarms <= 10,
            "false alarms {false_alarms}% above gate"
        );
    }

    #[test]
    fn ref_matches_target_boundary_checks() {
        assert!(ref_matches_target("src/query/impact.rs", "impact.rs"));
        assert!(ref_matches_target("./a/b.rs", "a/b.rs"));
        assert!(ref_matches_target("crate::memory::guard", "guard"));
        assert!(ref_matches_target("Context.render", "render"));
        assert!(!ref_matches_target("src/query/my_impact.rs", "impact.rs"));
        assert!(!ref_matches_target("src/query/impact.rs", ""));
        // Extension-only and middle-segment tokens must not match.
        assert!(!ref_matches_target("src/a.rs", "rs"));
        assert!(!ref_matches_target("crate::memory::guard", "memory"));
        assert!(!ref_matches_target("a.b.c", "b"));
    }

    fn run_collect(conn: &rusqlite::Connection, kinds: &[&str], limit: usize) -> Vec<String> {
        let (mut seen, mut out) = (HashSet::new(), Vec::new());
        collect_guards(
            conn,
            &GuardQuery(1000, &["src/a.rs"], &[], kinds, limit),
            &mut seen,
            &mut out,
        );
        out.into_iter().map(|g| g.key).collect()
    }

    #[test]
    fn collect_guards_orders_filters_and_falls_back_on_v1() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../schema/memory.sql"))
            .unwrap();
        conn.execute_batch(
            "INSERT INTO memories (key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, anchored_at, expires_at) VALUES
             ('d1', 1, 'decision', 'durable', '', 'first', '', 'src/a.rs', 1, 'cli', 100, 0, 0),
             ('g1', 1, 'gotcha',   'durable', '', 'anchored', '', 'src/a.rs', 2, 'cli', 50, 300, 0),
             ('d2', 1, 'decision', 'durable', '', 'newer', '', 'src/a.rs', 3, 'cli', 200, 0, 0);",
        ).unwrap();
        assert_eq!(run_collect(&conn, &["decision", "gotcha"], 2), ["g1", "d2"]);
        assert_eq!(run_collect(&conn, &["decision"], 1), vec!["d2"]);

        let v1 = rusqlite::Connection::open_in_memory().unwrap();
        v1.execute_batch(
            "CREATE TABLE memories (id INTEGER PRIMARY KEY, key TEXT, rev INTEGER, kind TEXT, scope TEXT, session TEXT, body TEXT, tags TEXT, refs TEXT, content_hash INTEGER, source TEXT, created_at INTEGER, expires_at INTEGER, superseded_by TEXT, superseded_at INTEGER);
             INSERT INTO memories (key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, expires_at) VALUES
             ('k-old', 1, 'decision', 'durable', '', 'older', '', 'src/a.rs', 1, 'cli', 10, 0),
             ('k-new', 1, 'decision', 'durable', '', 'newer', '', 'src/a.rs', 2, 'cli', 20, 0);",
        ).unwrap();
        assert_eq!(run_collect(&v1, &["decision"], 2), vec!["k-new", "k-old"]);
    }
}
