//! Ranked-memory model: candidate scoring features, min-max fusion, and
//! row rendering for mem query output.

/// Fusion weights for the three ranked-candidate features (lexical token
/// overlap, ref-token overlap, bounded recency decay). Tuning constants of
/// `fuse` alone, private to this module.
const W_LEX: f64 = 0.5;
const W_REF: f64 = 0.3;
const W_REC: f64 = 0.2;
/// Weight of the call-graph expansion signal. Adding a fourth feature leaves
/// the lexical/ref/recency ordering untouched when no graph candidate exists:
/// an all-zero feature min-max-normalizes to the design's all-equal 1.0, which
/// is a constant score offset for every row.
const W_GRAPH: f64 = 0.25;
/// Weight of the semantic-similarity feature (eg-judge text model). Activates
/// whenever at least one candidate carries an embedding; unembedded candidates
/// receive a neutral `0.5` so partial coverage does not drop the signal.
/// Weight 0.25 mirrors the graph signal: a secondary evidence term, never a
/// replacement for the lexical rank.
const W_SEM: f64 = 0.25;

/// One candidate row with its raw signals. `lex_raw` is the FTS5 bm25 rank
/// (`Some(bm25)` for FTS hits, smaller is better; `None` for ref/graph-only
/// hits, which receive `lex = 0.0`); `ref_raw` is the token-overlap fraction in
/// [0,1]; `graph_raw` is the call-graph decay `0.5^depth` (0 when not
/// reachable); `age_days` feeds the bounded recency decay.
pub struct Candidate {
    pub row: crate::db::MemRow,
    pub lex_raw: Option<f64>,
    pub ref_raw: f64,
    pub graph_raw: f64,
    /// Cosine(query, note) through the eg-judge text model; `None` when the
    /// model is not configured or this candidate has no embedding.
    pub sem_raw: Option<f64>,
    pub age_days: f64,
    /// Fused score, filled by [`fuse`].
    pub score: f64,
    pub is_global: bool,
    /// True when the FTS index produced this candidate (the only direct
    /// evidence that the query lexically matched the note).
    pub from_fts: bool,
}

fn bounds<I: Iterator<Item = f64>>(iter: I) -> (f64, f64) {
    iter.fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), v| {
        (min.min(v), max.max(v))
    })
}

fn norm(v: f64, (min, max): (f64, f64), invert: bool) -> f64 {
    if !min.is_finite() || !max.is_finite() || (max - min).abs() < f64::EPSILON {
        1.0
    } else {
        let n = (v - min) / (max - min);
        if invert {
            1.0 - n
        } else {
            n
        }
    }
}

/// Normalize `lex_raw` within one store before merging local & global
/// candidates, so corpus-dependent FTS5 `bm25` scales are comparable across
/// stores. Maps `Some(raw)` to `1.0 - norm(raw, bounds, true)` in `[0.0, 1.0]`
/// (smaller = better, preserving `fuse`'s inverted-bm25 convention).
pub fn normalize_lex_per_store(candidates: &mut [Candidate]) {
    let lex_b = bounds(candidates.iter().filter_map(|c| c.lex_raw));
    for c in candidates {
        if let Some(raw) = c.lex_raw {
            c.lex_raw = Some(1.0 - norm(raw, lex_b, true));
        }
    }
}

/// Fuse the candidates: invert bm25 (smaller is better; `None` -> `0.0`),
/// min-max both bounded signals, apply the recency decay, weight, then sort by
/// (score desc, created_at desc, key asc, rev desc). Deterministic: same
/// rows + same query + same binary -> same order.
pub fn fuse(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    let lex_b = bounds(candidates.iter().filter_map(|c| c.lex_raw));
    let ref_b = bounds(candidates.iter().map(|c| c.ref_raw));
    let graph_b = bounds(candidates.iter().map(|c| c.graph_raw));
    let sem_active = candidates.iter().any(|c| c.sem_raw.is_some());
    let sem_b = bounds(candidates.iter().filter_map(|c| c.sem_raw));
    for c in &mut candidates {
        let rec = 1.0 / (1.0 + c.age_days);
        let lex = c.lex_raw.map_or(0.0, |raw| norm(raw, lex_b, true));
        let reference = norm(c.ref_raw, ref_b, false);
        let graph = norm(c.graph_raw, graph_b, false);
        c.score = W_LEX * lex + W_REF * reference + W_REC * rec + W_GRAPH * graph;
        if sem_active {
            let sem = c.sem_raw.map_or(0.5, |v| norm(v, sem_b, false));
            c.score += W_SEM * sem;
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.row.created_at.cmp(&a.row.created_at))
            .then(a.row.key.cmp(&b.row.key))
            .then(b.row.rev.cmp(&a.row.rev))
    });
    candidates
}

// ---------------------------------------------------------------------------
// Rendering (shared bytes for CLI and MCP).
// ---------------------------------------------------------------------------

/// One note row header line: `key rev kind scope created_at`.
fn render_row_header(row: &crate::db::MemRow) -> String {
    format!(
        "{} r{} {} {} {}",
        row.key, row.rev, row.kind, row.scope, row.created_at
    )
}

/// Render a row with a trailing header tag (the `--include-superseded`
/// listing marks replaced revisions with ` [superseded]`).
pub fn render_row_tagged(row: &crate::db::MemRow, tag: &str) -> String {
    let mut out = render_row_header(row);
    out.push_str(tag);
    for line in row.body.lines() {
        out.push_str(&format!("\n  {line}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_cand(id: i64, lex_raw: Option<f64>, sem_raw: Option<f64>, graph_raw: f64) -> Candidate {
        Candidate {
            row: crate::db::MemRow {
                id,
                key: format!("k{id}"),
                rev: 1,
                kind: "fact".into(),
                scope: "durable".into(),
                session: String::new(),
                body: "b".into(),
                tags: String::new(),
                refs: String::new(),
                source: "cli".into(),
                created_at: 100,
                anchored_at: 0,
                expires_at: 0,
                superseded_by: None,
            },
            lex_raw,
            ref_raw: 0.0,
            graph_raw,
            sem_raw,
            age_days: 0.0,
            score: 0.0,
            is_global: false,
            from_fts: lex_raw.is_some(),
        }
    }

    #[test]
    fn none_lex_raw_gets_zero_without_distorting_bm25_bounds() {
        let fused = fuse(vec![
            mk_cand(1, Some(-5.0), None, 0.0),
            mk_cand(2, Some(-1.0), None, 0.0),
            mk_cand(3, None, None, 0.5),
        ]);
        let by_id: std::collections::HashMap<i64, f64> =
            fused.iter().map(|c| (c.row.id, c.score)).collect();
        // Candidate 1 is best FTS (lex=1.0), candidate 2 is worst FTS (lex=0.0).
        assert!(by_id[&1] > by_id[&2]);
        // Candidate 3 (None lex_raw -> lex=0.0, graph=1.0) must not compress FTS range.
        assert!((by_id[&1] - by_id[&2] - W_LEX).abs() < 1e-9);
    }

    #[test]
    fn partial_embeddings_assign_neutral_half_to_unembedded() {
        let fused = fuse(vec![
            mk_cand(1, Some(-2.0), Some(0.9), 0.0),
            mk_cand(2, Some(-2.0), Some(0.1), 0.0),
            mk_cand(3, Some(-2.0), None, 0.0),
        ]);
        // Ranked: high sem (1.0) > unembedded neutral (0.5) > low sem (0.0).
        let ids: Vec<i64> = fused.iter().map(|c| c.row.id).collect();
        assert_eq!(ids, vec![1, 3, 2]);
    }
}
