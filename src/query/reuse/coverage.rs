//! Phase 4 opt-in coverage detectors. All results are advisory, capped at
//! Medium confidence, and never included in the default scan.

use std::collections::{HashMap, HashSet};

use super::corpus::{load_callees, load_symbols, SymbolData};
use super::guards::{is_more_canonical, language_family, LangFamily};
use super::tokens::{build_name_stoplist, build_stoplist, is_stoplisted, tokenize_name_sig};
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoverageKind {
    Class,
    Unresolved,
    Tests,
}

impl CoverageKind {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "class" => Some(Self::Class),
            "unresolved" => Some(Self::Unresolved),
            "tests" => Some(Self::Tests),
            _ => None,
        }
    }
}

pub(super) fn analyze(
    conn: &Connection,
    kinds: &[CoverageKind],
    path_filter: Option<&[String]>,
) -> Result<Vec<ReuseCandidate>> {
    let mut pool = Vec::new();
    if kinds.contains(&CoverageKind::Class) {
        pool.extend(class_candidates(conn, path_filter)?);
    }
    let need_unresolved = kinds.contains(&CoverageKind::Unresolved);
    let need_tests = kinds.contains(&CoverageKind::Tests);
    if need_unresolved || need_tests {
        let mut symbols = load_symbols(conn, false)?;
        if need_unresolved {
            let _ = load_callees(conn, &mut symbols)?;
            let stopset = build_stoplist(&symbols);
            for sym in symbols.values_mut() {
                sym.all_callees
                    .retain(|c| !stopset.contains(c.as_str()) || is_raw_client(c.as_str()));
                sym.unresolved_callees
                    .retain(|c| !stopset.contains(c.as_str()));
                sym.resolved_callees
                    .retain(|c| !stopset.contains(c.as_str()));
            }
            pool.extend(unresolved_candidates(&symbols, path_filter)?);
        }
        if need_tests {
            let name_stopset = build_name_stoplist(&symbols);
            for sym in symbols.values_mut() {
                sym.name_tokens
                    .retain(|token| !name_stopset.contains(token));
            }
            pool.extend(test_candidates(conn, &symbols, path_filter)?);
        }
    }
    pool.sort_by(|a, b| {
        b.confidence
            .rank()
            .cmp(&a.confidence.rank())
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.canonical.cmp(&b.canonical))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.canonical_file.cmp(&b.canonical_file))
            .then_with(|| a.canonical_line.cmp(&b.canonical_line))
    });
    Ok(pool)
}

/// D5 is pairwise; cap the materialized pool so thousands of same-named
/// classes cannot exhaust memory before finalize truncates the output.
const MAX_CLASS_CANDIDATES: usize = 10_000;

fn is_boilerplate_method(name: &str) -> bool {
    matches!(
        name,
        "__init__" | "new" | "default" | "clone" | "fmt" | "constructor"
    )
}

struct ClassRow {
    name: String,
    qualname: String,
    file_id: i64,
    file_path: String,
    line_start: i64,
    line_end: i64,
}

fn class_candidates(
    conn: &Connection,
    path_filter: Option<&[String]>,
) -> Result<Vec<ReuseCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT name, qualname, file_id, file_path, line_start, line_end
         FROM symbols WHERE kind = 'class' AND is_test = 0
         ORDER BY qualname, file_path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ClassRow {
            name: r.get(0)?,
            qualname: r.get(1)?,
            file_id: r.get(2)?,
            file_path: r.get(3)?,
            line_start: r.get(4)?,
            line_end: r.get(5)?,
        })
    })?;
    let mut buckets: HashMap<(String, LangFamily), Vec<ClassRow>> = HashMap::new();
    for row in rows {
        let cls = row?;
        let key = (cls.name.clone(), language_family(&cls.file_path));
        buckets.entry(key).or_default().push(cls);
    }

    let mut methods: HashMap<(i64, String), HashSet<String>> = HashMap::new();
    let mut method_stmt = conn.prepare(
        "SELECT file_id, qualname, name FROM symbols WHERE kind = 'method' AND is_test = 0",
    )?;
    let method_rows = method_stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in method_rows {
        let (file_id, qualname, name) = row?;
        if is_boilerplate_method(&name) {
            continue;
        }
        if let Some((class_qualname, _)) = qualname.rsplit_once('.') {
            methods
                .entry((file_id, class_qualname.to_string()))
                .or_default()
                .insert(name);
        }
    }

    let mut pool = Vec::new();
    let mut bucket_keys: Vec<(String, LangFamily)> = buckets
        .iter()
        .filter(|(_, v)| v.len() >= 2)
        .map(|(k, _)| k.clone())
        .collect();
    bucket_keys.sort_by(|a, b| a.0.cmp(&b.0));

    'classes: for key in bucket_keys {
        let group = &buckets[&key];
        for (i, left) in group.iter().enumerate() {
            let Some(left_methods) = methods.get(&(left.file_id, left.qualname.clone())) else {
                continue;
            };
            for right in group.iter().skip(i + 1) {
                if left.file_path == right.file_path {
                    continue;
                }
                if let Some(paths) = path_filter {
                    if !super::target::path_matches(&left.file_path, paths)
                        && !super::target::path_matches(&right.file_path, paths)
                    {
                        continue;
                    }
                }
                let Some(right_methods) = methods.get(&(right.file_id, right.qualname.clone()))
                else {
                    continue;
                };
                let mut shared: Vec<String> =
                    left_methods.intersection(right_methods).cloned().collect();
                if shared.is_empty() {
                    continue;
                }
                shared.sort();
                let confidence = if shared.len() >= 2 {
                    ReuseConfidence::Medium
                } else {
                    ReuseConfidence::Low
                };
                pool.push(ReuseCandidate {
                    detector: DetectorId::D5,
                    symbol: right.qualname.clone(),
                    file: right.file_path.clone(),
                    line: right.line_start,
                    line_end: right.line_end,
                    canonical: left.qualname.clone(),
                    canonical_file: left.file_path.clone(),
                    canonical_line: left.line_start,
                    canonical_line_end: left.line_end,
                    canonical_callers: 0,
                    overlap_pct: None,
                    shared_items: shared,
                    raw_client: None,
                    confidence,
                    reasons: vec![ReasonCode::CoverageClass],
                });
                if pool.len() >= MAX_CLASS_CANDIDATES {
                    break 'classes;
                }
            }
        }
    }
    Ok(pool)
}

fn is_generic_coverage_name(name: &str) -> bool {
    matches!(
        name,
        "default" | "new" | "call" | "apply" | "then" | "map" | "get" | "set" | "run"
    )
}

fn unresolved_candidates(
    symbols: &HashMap<i64, SymbolData>,
    path_filter: Option<&[String]>,
) -> Result<Vec<ReuseCandidate>> {
    let mut by_name: HashMap<(&str, LangFamily), &SymbolData> = HashMap::new();
    for symbol in symbols.values() {
        let key = (symbol.name.as_str(), symbol.lang_family);
        by_name
            .entry(key)
            .and_modify(|current| {
                if is_more_canonical(symbol, current) {
                    *current = symbol;
                }
            })
            .or_insert(symbol);
    }
    let mut pool = Vec::new();
    for target in symbols.values() {
        if let Some(paths) = path_filter {
            if !super::target::path_matches(&target.file_path, paths) {
                continue;
            }
        }
        for callee in &target.unresolved_callees {
            if is_generic_coverage_name(callee) || is_stoplisted(callee) {
                continue;
            }
            let Some(canonical) = by_name.get(&(callee.as_str(), target.lang_family)).copied()
            else {
                continue;
            };
            if canonical.id == target.id
                || canonical.file_id == target.file_id
                || target.lang_family != canonical.lang_family
            {
                continue;
            }
            pool.push(ReuseCandidate {
                detector: DetectorId::D1L,
                symbol: target.qualname.clone(),
                file: target.file_path.clone(),
                line: target.line_start,
                line_end: target.line_end,
                canonical: canonical.qualname.clone(),
                canonical_file: canonical.file_path.clone(),
                canonical_line: canonical.line_start,
                canonical_line_end: canonical.line_end,
                canonical_callers: canonical.inbound_callers,
                overlap_pct: None,
                shared_items: vec![callee.clone()],
                raw_client: None,
                confidence: ReuseConfidence::Low,
                reasons: vec![ReasonCode::CoverageUnresolved],
            });
        }
    }
    Ok(pool)
}

fn test_candidates(
    conn: &Connection,
    production: &HashMap<i64, SymbolData>,
    path_filter: Option<&[String]>,
) -> Result<Vec<ReuseCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT name, qualname, file_path, line_start, line_end, signature
         FROM symbols WHERE is_test = 1 AND kind IN ('function', 'method')
         ORDER BY qualname, file_path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut by_token: HashMap<&str, Vec<&SymbolData>> = HashMap::new();
    for candidate in production.values() {
        for token in &candidate.name_tokens {
            by_token.entry(token.as_str()).or_default().push(candidate);
        }
    }
    let mut pool = Vec::new();
    for row in rows {
        let (name, qualname, file, line, line_end, signature) = row?;
        if let Some(paths) = path_filter {
            if !super::target::path_matches(&file, paths) {
                continue;
            }
        }
        let (all_tokens, _, name_tokens) = tokenize_name_sig(&name, &signature, true);
        let name_tokens = if name_tokens.is_empty() {
            all_tokens
        } else {
            name_tokens
        };
        if name_tokens.len() < 3 {
            continue;
        }
        let test_family = language_family(&file);
        let mut counts: HashMap<i64, (&SymbolData, usize)> = HashMap::new();
        for token in &name_tokens {
            if let Some(candidates) = by_token.get(token.as_str()) {
                for &candidate in candidates {
                    if candidate.lang_family != test_family {
                        continue;
                    }
                    counts
                        .entry(candidate.id)
                        .and_modify(|(_, c)| *c += 1)
                        .or_insert((candidate, 1));
                }
            }
        }
        let mut best: Option<(&SymbolData, usize)> = None;
        for (candidate, shared) in counts.into_values() {
            if shared < 3 {
                continue;
            }
            let better = best.is_none_or(|(current, current_shared)| {
                shared > current_shared
                    || (shared == current_shared && is_more_canonical(candidate, current))
            });
            if better {
                best = Some((candidate, shared));
            }
        }
        let Some((canonical, shared)) = best else {
            continue;
        };
        pool.push(ReuseCandidate {
            detector: DetectorId::DT,
            symbol: qualname,
            file,
            line,
            line_end,
            canonical: canonical.qualname.clone(),
            canonical_file: canonical.file_path.clone(),
            canonical_line: canonical.line_start,
            canonical_line_end: canonical.line_end,
            canonical_callers: canonical.inbound_callers,
            overlap_pct: None,
            shared_items: vec![format!("{shared} shared tokens")],
            raw_client: None,
            confidence: ReuseConfidence::Low,
            reasons: vec![ReasonCode::CoverageTests],
        });
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../schema/index.sql"))
            .unwrap();
        for path in ["a.py", "b.py", "c.py"] {
            conn.execute(
                "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
                 VALUES (NULL, ?1, 10, 1, 1, 'ok', 0)",
                [path],
            )
            .unwrap();
        }
        for (id, name, qualname, file_id, file_path, line_end) in [
            (1, "fetch", "a.fetch", 1, "a.py", 2),
            (2, "send_data", "b.send_data", 2, "b.py", 4),
            (3, "unwrap", "c.unwrap", 3, "c.py", 2),
        ] {
            conn.execute(
                "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end, signature)
                 VALUES (?1, 'function', ?2, ?3, ?4, ?5, 1, ?6, '')",
                rusqlite::params![id, name, qualname, file_id, file_path, line_end],
            )
            .unwrap();
        }
        for (dst_name, site_line) in [("fetch", 2), ("unwrap", 3)] {
            conn.execute(
                "INSERT INTO unresolved_calls(file_id, dst_name, kind, site_line)
                 VALUES (2, ?1, 'call', ?2)",
                rusqlite::params![dst_name, site_line],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn coverage_raw_client_still_matches_user_defined_symbol() {
        // review.md I-11.1 attributed D1L raw-client matches to the retain's
        // `|| is_raw_client` disjunct; that disjunct is unreachable dead code
        // (build_stoplist never stoplists a raw client, and the static
        // STOPLIST is disjoint from RAW_CLIENTS), and the same-name match
        // below is a true positive by design — a user-defined `fetch` wrapper
        // is exactly what D1L should surface, the same reason D4 keeps raw
        // clients on `all_callees`. Pin both directions of the retain: a
        // raw-client-named callee with a same-family defining symbol still
        // reaches unresolved_candidates, while a stoplisted callee does not.
        let conn = index_fixture();
        let pool = analyze(&conn, &[CoverageKind::Unresolved], None).unwrap();
        let matched: Vec<(String, String)> = pool
            .iter()
            .filter(|c| c.detector == DetectorId::D1L && c.symbol == "b.send_data")
            .map(|c| (c.canonical.clone(), c.shared_items.join(",")))
            .collect();
        assert!(
            matched.contains(&("a.fetch".into(), "fetch".into())),
            "user-defined fetch must stay a D1L match: {matched:?}"
        );
        assert!(
            !matched.iter().any(|(canonical, _)| canonical == "c.unwrap"),
            "stoplisted callees must be dropped from unresolved_callees: {matched:?}"
        );
    }

    #[test]
    fn coverage_class_excludes_boilerplate_and_honors_scope() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../schema/index.sql"))
            .unwrap();
        for (id, path) in [(1, "a.py"), (2, "b.py"), (3, "c.py")] {
            conn.execute(
                "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
                 VALUES (?1, ?2, 10, 1, 1, 'ok', 0)",
                rusqlite::params![id, path],
            )
            .unwrap();
        }
        for (id, kind, name, qualname, file_id, file_path) in [
            (1, "class", "Widget", "a.Widget", 1, "a.py"),
            (2, "method", "__init__", "a.Widget.__init__", 1, "a.py"),
            (3, "class", "Widget", "b.Widget", 2, "b.py"),
            (4, "method", "__init__", "b.Widget.__init__", 2, "b.py"),
            (5, "class", "Widget", "c.Widget", 3, "c.py"),
            (6, "method", "render", "a.Widget.render", 1, "a.py"),
            (7, "method", "render", "c.Widget.render", 3, "c.py"),
        ] {
            conn.execute(
                "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 2, '')",
                rusqlite::params![id, kind, name, qualname, file_id, file_path],
            )
            .unwrap();
        }
        // Only __init__ shared between a.Widget and b.Widget -> must be excluded.
        let all = analyze(&conn, &[CoverageKind::Class], None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].symbol, "c.Widget");
        assert_eq!(all[0].canonical, "a.Widget");

        // Scope to b.py -> no matches because b.Widget only shared __init__.
        let scoped = analyze(&conn, &[CoverageKind::Class], Some(&["b.py".into()])).unwrap();
        assert!(scoped.is_empty(), "{scoped:?}");
    }
}
