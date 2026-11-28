//! Read-only call-graph expansion for memory ranking.
//!
//! `mem query` ranks notes by lexical and ref evidence. When a code index is
//! available, this module also follows resolved `calls` edges outward from the
//! symbols named by the query, so a note attached to a downstream callee still
//! surfaces. The index is opened read-only by the service layer; every SQL
//! error here degrades to the plain FTS ranking (no error path escapes).

use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::Connection;

use crate::db::{MemFilter, MemRow};

/// Bound the seed, traversal, and ref-name work so a busy query stays inside
/// the latency gate even on a large index.
const MAX_SEEDS: usize = 32;
const MAX_VISITED: usize = 256;
const MAX_DEPTH: usize = 3;
const MAX_NAMES: usize = 128;
const NEIGHBOR_FANOUT: usize = 64;

/// Graph evidence found for one query: decay scores for rows already in the
/// lexical candidate set, and rows the graph adds.
#[derive(Debug, Default)]
pub(crate) struct GraphExpansion {
    pub(crate) scores: HashMap<i64, f64>,
    pub(crate) extra: Vec<(MemRow, f64)>,
}

/// Follow `calls` edges from the symbols named by the query tokens and return
/// notes whose `refs` point at any reachable symbol. `known` holds the row ids
/// the lexical pass already found; those receive a score without duplication.
pub(crate) fn graph_expand(
    index: &Connection,
    memory: &Connection,
    query_tokens: &BTreeSet<String>,
    filter: &MemFilter,
    known: &HashSet<i64>,
    cap: usize,
) -> GraphExpansion {
    let seeds = match seed_symbols(index, query_tokens) {
        Ok(seeds) => seeds,
        Err(_) => return GraphExpansion::default(),
    };
    if seeds.is_empty() {
        return GraphExpansion::default();
    }
    let depth_by_id = match bfs_depth(index, &seeds) {
        Ok(depths) => depths,
        Err(_) => return GraphExpansion::default(),
    };
    let names = match related_names(index, &depth_by_id) {
        Ok(names) => names,
        Err(_) => return GraphExpansion::default(),
    };
    if names.is_empty() {
        return GraphExpansion::default();
    }
    let tokens: Vec<String> = names.keys().cloned().collect();
    let rows = match crate::db::memory_ref_candidate_rows(
        memory,
        &tokens,
        &filter.window,
        filter.now,
        cap,
    ) {
        Ok(rows) => rows,
        Err(_) => return GraphExpansion::default(),
    };

    let mut scores: HashMap<i64, f64> = HashMap::new();
    let mut extra: HashMap<i64, (MemRow, f64)> = HashMap::new();
    for row in rows {
        if !crate::db::memory_row_is_current(&row, filter) {
            continue;
        }
        let mut best_decay = 0.0_f64;
        for (name, decay) in &names {
            if crate::memory::ref_matches_target(&row.refs, name) {
                best_decay = best_decay.max(*decay);
            }
        }
        if best_decay <= 0.0 {
            continue;
        }
        if known.contains(&row.id) {
            scores.insert(row.id, best_decay);
            continue;
        }
        extra
            .entry(row.id)
            .and_modify(|(_, decay)| *decay = decay.max(best_decay))
            .or_insert((row, best_decay));
    }

    let mut extra: Vec<(MemRow, f64)> = extra.into_values().collect();
    extra.sort_by(|(a_row, a_score), (b_row, b_score)| {
        b_score
            .partial_cmp(a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b_row.created_at.cmp(&a_row.created_at))
            .then(a_row.key.cmp(&b_row.key))
    });
    GraphExpansion { scores, extra }
}

/// Symbol ids matching the query tokens through the symbol FTS index. The
/// memory tokenizer splits `target_entry` into `target entry`, so exact token
/// matching (no prefix wildcard) is the bridge between a memory query and an
/// identifier without fanning out to every symbol that starts with the same
/// letters.
fn seed_symbols(index: &Connection, query_tokens: &BTreeSet<String>) -> rusqlite::Result<Vec<i64>> {
    if query_tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut expanded = BTreeSet::new();
    for token in query_tokens {
        let clean = token.replace('"', "");
        if !clean.is_empty() {
            expanded.insert(clean.clone());
        }
        for sub in crate::extract::split_ident(token).split_whitespace() {
            let clean_sub = sub.replace('"', "");
            if !clean_sub.is_empty() {
                expanded.insert(clean_sub);
            }
        }
    }
    if expanded.is_empty() {
        return Ok(Vec::new());
    }
    let expr = expanded
        .iter()
        .take(MAX_SEEDS)
        .map(|token| format!("\"{token}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut stmt = index.prepare(
        "SELECT s.id FROM symbols_fts ft
         JOIN symbols s ON s.id = ft.rowid
         WHERE symbols_fts MATCH ?1
         ORDER BY rank, s.id LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![expr, MAX_SEEDS as i64], |r| {
        r.get::<_, i64>(0)
    })?;
    rows.collect()
}

/// Breadth-first distance (capped) from the seed symbols over resolved calls,
/// batched per BFS level (`WHERE src_id IN (...)`).
fn bfs_depth(index: &Connection, seeds: &[i64]) -> rusqlite::Result<HashMap<i64, usize>> {
    let mut depth: HashMap<i64, usize> = seeds.iter().map(|id| (*id, 0)).collect();
    let mut frontier: Vec<i64> = seeds.to_vec();
    for level in 1..=MAX_DEPTH {
        frontier.sort_unstable();
        frontier.dedup();
        let mut next: Vec<i64> = Vec::new();
        for chunk in frontier.chunks(MAX_VISITED) {
            let placeholders = crate::db::sql_placeholders(chunk.len());
            let sql = format!(
                "SELECT DISTINCT src_id, dst_id FROM edges
                 WHERE src_id IN ({placeholders}) AND kind = 'calls' AND dst_id IS NOT NULL
                 ORDER BY src_id, dst_id"
            );
            let mut stmt = index.prepare(&sql)?;
            let neighbors = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;
            let mut cur_src: Option<i64> = None;
            let mut per_src = 0usize;
            for neighbor in neighbors {
                let (src_id, dst_id) = neighbor?;
                if cur_src != Some(src_id) {
                    cur_src = Some(src_id);
                    per_src = 0;
                }
                if per_src >= NEIGHBOR_FANOUT {
                    continue;
                }
                per_src += 1;
                if depth.contains_key(&dst_id) {
                    continue;
                }
                depth.insert(dst_id, level);
                next.push(dst_id);
                if depth.len() >= MAX_VISITED {
                    return Ok(depth);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(depth)
}

/// Map each reachable symbol name/qualname to its decay `0.5^depth`, keeping
/// the strongest (smallest) depth when two symbols share a short name.
/// Iterates `(depth, id)` in deterministic order before capping at `MAX_NAMES`.
fn related_names(
    index: &Connection,
    depth_by_id: &HashMap<i64, usize>,
) -> rusqlite::Result<HashMap<String, f64>> {
    let mut pairs: Vec<(usize, i64)> = depth_by_id.iter().map(|(&id, &d)| (d, id)).collect();
    pairs.sort_unstable();
    pairs.truncate(MAX_VISITED);
    if pairs.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<i64> = pairs.iter().map(|&(_, id)| id).collect();
    let placeholders = crate::db::sql_placeholders(ids.len());
    let sql = format!("SELECT id, name, qualname FROM symbols WHERE id IN ({placeholders})");
    let mut stmt = index.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut by_id: HashMap<i64, (String, String)> = HashMap::new();
    for row in rows {
        let (id, name, qualname) = row?;
        by_id.insert(id, (name, qualname));
    }
    let mut names: HashMap<String, f64> = HashMap::new();
    for (depth, id) in pairs {
        let Some((name, qualname)) = by_id.remove(&id) else {
            continue;
        };
        let decay = 0.5_f64.powi(depth as i32);
        let candidates = (name.len() >= 4)
            .then_some(name)
            .into_iter()
            .chain(std::iter::once(qualname));
        for label in candidates {
            if names.len() >= MAX_NAMES && !names.contains_key(&label) {
                continue;
            }
            names
                .entry(label)
                .and_modify(|best| *best = best.max(decay))
                .or_insert(decay);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn decay_halves_per_hop() {
        assert_eq!(0.5_f64.powi(0), 1.0);
        assert_eq!(0.5_f64.powi(1), 0.5);
        assert_eq!(0.5_f64.powi(2), 0.25);
    }

    #[test]
    fn graph_expand_finds_downstream_note_within_10ms() {
        let index = Connection::open_in_memory().unwrap();
        index
            .execute_batch(include_str!("../../schema/index.sql"))
            .unwrap();
        index
            .execute_batch(include_str!("../../schema/indexes.sql"))
            .unwrap();
        index
            .execute(
                "INSERT INTO files
                 (id, path, size, mtime, content_hash, parse_status, error_nodes, unresolved)
                 VALUES (1, 'f.py', 0, 0, 0, 'ok', 0, 0)",
                [],
            )
            .unwrap();
        let tx = index.unchecked_transaction().unwrap();
        for i in 0..1_000_i64 {
            tx.execute(
                "INSERT INTO symbols
                 (id, kind, name, qualname, file_id, file_path, line_start, line_end, signature, is_test)
                 VALUES (?1, 'function', ?2, ?3, 1, 'f.py', 1, 2, '', 0)",
                rusqlite::params![i, format!("sym{i}"), format!("m.sym{i}")],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO symbols_fts(rowid, text) VALUES (?1, ?2)",
                rusqlite::params![i, format!("sym{i}")],
            )
            .unwrap();
            if i > 0 {
                tx.execute(
                    "INSERT INTO edges (src_id, dst_id, dst_name, kind, confidence, site_line)
                     VALUES (?1, ?2, ?3, 'calls', 'resolved', 0)",
                    rusqlite::params![i - 1, i, format!("sym{i}")],
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();

        let memory = Connection::open_in_memory().unwrap();
        memory
            .execute_batch(include_str!("../../schema/memory.sql"))
            .unwrap();
        memory
            .execute(
                "INSERT INTO memories
                 (key, rev, kind, scope, session, body, tags, refs, content_hash, source, created_at, expires_at)
                 VALUES ('downstream', 1, 'fact', 'durable', '', 'downstream effect', '', 'sym3', 0, 'cli', 1, 0)",
                [],
            )
            .unwrap();

        let filter = crate::db::MemFilter {
            kind: None,
            scope: None,
            now: 10,
            include_superseded: false,
            window: crate::memory::TimeWindow::default(),
        };
        let tokens: BTreeSet<String> = ["sym0".to_string()].into_iter().collect();
        let start = Instant::now();
        let expansion = graph_expand(&index, &memory, &tokens, &filter, &HashSet::new(), 32);
        let elapsed = start.elapsed();
        println!("graph expansion on 1,000 symbols: {elapsed:?}");
        assert!(
            expansion.extra.iter().any(
                |(row, decay)| row.key == "downstream" && (*decay - 0.125).abs() < f64::EPSILON
            ),
            "expected the depth-3 note with 0.125 decay: {:?}",
            expansion.extra
        );
        assert!(
            elapsed < Duration::from_millis(10),
            "C4 latency gate: {elapsed:?} >= 10ms"
        );
    }

    #[test]
    fn related_names_is_deterministic_and_skips_short_bare_names() {
        let index = Connection::open_in_memory().unwrap();
        index
            .execute_batch(include_str!("../../schema/index.sql"))
            .unwrap();
        index
            .execute(
                "INSERT INTO files (id, path, size, mtime, content_hash, parse_status, error_nodes, unresolved)
                 VALUES (1, 'f.py', 0, 0, 0, 'ok', 0, 0)",
                [],
            )
            .unwrap();
        for (id, name, qual) in [
            (1_i64, "fn1", "mod.fn1"),
            (2_i64, "long_name", "mod.long_name"),
        ] {
            index
                .execute(
                    "INSERT INTO symbols (id, kind, name, qualname, file_id, file_path, line_start, line_end, signature, is_test)
                     VALUES (?1, 'function', ?2, ?3, 1, 'f.py', 1, 2, '', 0)",
                    rusqlite::params![id, name, qual],
                )
                .unwrap();
        }
        let mut depth_by_id = HashMap::new();
        depth_by_id.insert(1, 1);
        depth_by_id.insert(2, 2);
        let first = related_names(&index, &depth_by_id).unwrap();
        let second = related_names(&index, &depth_by_id).unwrap();
        assert_eq!(first, second);
        // Bare name < 4 chars ("fn1") is excluded while qualname ("mod.fn1") is kept.
        assert!(!first.contains_key("fn1"));
        assert!(first.contains_key("mod.fn1"));
        assert!(first.contains_key("long_name"));
    }
}
