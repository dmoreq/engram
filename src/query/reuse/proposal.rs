//! Phase 2 pre-finalize proposal. A proposed signature and optional callee
//! list are matched against indexed production symbols without creating a
//! file. Candidates are derived only from resolved corpus data.

use std::collections::HashSet;

use super::corpus::{load_callees, load_symbols};
use super::guards::LangFamily;
use super::tokens::{build_name_stoplist, build_stoplist, tokenize_name_sig};
use super::*;

pub(super) fn analyze(conn: &Connection, opts: &ReuseOpts) -> Result<Vec<ReuseCandidate>> {
    let signature = opts
        .proposal_signature
        .as_deref()
        .ok_or_else(|| EgError::Usage("propose must be non-empty".to_string()))?;
    let name = proposal_name(signature)
        .ok_or_else(|| EgError::Usage("propose needs an identifier".to_string()))?;
    let (all_tokens, _type_tokens, mut name_tokens) = tokenize_name_sig(&name, signature, true);
    let mut proposed = callee_set(&opts.proposed_callees);
    let has_callees = !proposed.is_empty();
    let language = proposal_language(signature);

    let mut symbols = load_symbols(conn, true)?;
    if has_callees {
        let _ = load_callees(conn, &mut symbols)?;
        let stopset = build_stoplist(&symbols);
        for sym in symbols.values_mut() {
            sym.all_callees
                .retain(|c| !stopset.contains(c.as_str()) || is_raw_client(c.as_str()));
            sym.resolved_callees
                .retain(|c| !stopset.contains(c.as_str()));
        }
        let filtered_proposed: HashSet<String> = proposed
            .iter()
            .filter(|c| !stopset.contains(c.as_str()) || is_raw_client(c.as_str()))
            .cloned()
            .collect();
        if !filtered_proposed.is_empty() {
            proposed = filtered_proposed;
        }
    }
    let name_stopset = build_name_stoplist(&symbols);
    for sym in symbols.values_mut() {
        sym.name_tokens
            .retain(|token| !name_stopset.contains(token));
    }
    let filtered_name: HashSet<String> = name_tokens
        .iter()
        .filter(|token| !name_stopset.contains(token.as_str()))
        .cloned()
        .collect();
    if !filtered_name.is_empty() {
        name_tokens = filtered_name;
    }

    let mut pool = match_candidates(
        &symbols,
        &name,
        language,
        &proposed,
        &name_tokens,
        &all_tokens,
        has_callees,
    );
    // Token fallback when --callees has 0 resolved hits across the corpus
    // (unless those callees exist as unresolved-only calls, which Phase 2
    // intentionally ignores).
    if has_callees
        && pool.is_empty()
        && !symbols
            .values()
            .any(|s| !proposed.is_disjoint(&s.unresolved_callees))
    {
        pool = match_candidates(
            &symbols,
            &name,
            language,
            &proposed,
            &name_tokens,
            &all_tokens,
            false,
        );
    }

    pool.sort_by(|a, b| {
        b.confidence
            .rank()
            .cmp(&a.confidence.rank())
            .then_with(|| b.overlap_pct.cmp(&a.overlap_pct))
            .then_with(|| b.canonical_callers.cmp(&a.canonical_callers))
            .then_with(|| a.canonical.cmp(&b.canonical))
            .then_with(|| a.canonical_file.cmp(&b.canonical_file))
            .then_with(|| a.canonical_line.cmp(&b.canonical_line))
    });
    Ok(pool)
}

fn match_candidates(
    symbols: &std::collections::HashMap<i64, corpus::SymbolData>,
    name: &str,
    language: Option<LangFamily>,
    proposed: &HashSet<String>,
    name_tokens: &HashSet<String>,
    all_tokens: &HashSet<String>,
    use_callees: bool,
) -> Vec<ReuseCandidate> {
    let mut pool = Vec::new();
    for candidate in symbols.values() {
        if language.is_some_and(|family| family != candidate.lang_family) {
            continue;
        }
        let shared_callees: Vec<String> = if use_callees {
            proposed
                .intersection(&candidate.resolved_callees)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let candidate_tokens = if candidate.name_tokens.is_empty() {
            &candidate.tokens
        } else {
            &candidate.name_tokens
        };
        let proposed_tokens = if name_tokens.is_empty() {
            all_tokens
        } else {
            name_tokens
        };
        let shared_tokens: Vec<String> = proposed_tokens
            .intersection(candidate_tokens)
            .cloned()
            .collect();
        let union = proposed_tokens.union(candidate_tokens).count();
        let token_overlap = if union == 0 {
            0.0
        } else {
            shared_tokens.len() as f64 / union as f64
        };

        let (detector, pct, confidence, reasons, mut items) = if use_callees {
            let Some((det, pct, conf, reas)) = score_proposed_callees(
                &shared_callees,
                proposed.len(),
                candidate.resolved_callees.len(),
                token_overlap,
            ) else {
                continue;
            };
            (det, pct, conf, reas, shared_callees)
        } else {
            let Some((det, pct, conf, reas)) =
                score_proposed_tokens(shared_tokens.len(), token_overlap)
            else {
                continue;
            };
            (det, pct, conf, reas, shared_tokens)
        };

        items.sort();
        pool.push(ReuseCandidate {
            detector,
            symbol: name.to_string(),
            file: "<proposal>".to_string(),
            line: 0,
            line_end: 0,
            canonical: candidate.qualname.clone(),
            canonical_file: candidate.file_path.clone(),
            canonical_line: candidate.line_start,
            canonical_line_end: candidate.line_end,
            canonical_callers: candidate.inbound_callers,
            overlap_pct: Some(pct),
            shared_items: items,
            raw_client: None,
            confidence,
            reasons,
        });
    }
    pool
}

fn score_proposed_callees(
    shared_callees: &[String],
    proposed_len: usize,
    cand_callees_len: usize,
    token_overlap: f64,
) -> Option<(DetectorId, u64, ReuseConfidence, Vec<ReasonCode>)> {
    let coverage = shared_callees.len() as f64 / proposed_len as f64;
    let containment = if cand_callees_len == 0 {
        0.0
    } else {
        shared_callees.len() as f64 / cand_callees_len as f64
    };
    if shared_callees.is_empty() || coverage.max(containment) < 0.50 {
        return None;
    }
    let mut reasons = Vec::new();
    if coverage < 1.0 {
        reasons.push(ReasonCode::ProposalPartial);
    }
    let confidence = if shared_callees.len() < 2 {
        ReuseConfidence::Low
    } else if coverage >= 0.95 && token_overlap >= 0.50 {
        ReuseConfidence::High
    } else if coverage >= 0.75 {
        ReuseConfidence::Medium
    } else {
        ReuseConfidence::Low
    };
    Some((
        DetectorId::D1,
        (coverage.max(containment) * 100.0).round() as u64,
        confidence,
        reasons,
    ))
}

fn score_proposed_tokens(
    shared_len: usize,
    token_overlap: f64,
) -> Option<(DetectorId, u64, ReuseConfidence, Vec<ReasonCode>)> {
    // A single shared token counts when it is the whole of both sides: the
    // 1-token function (`fnv1a64`) is exactly the shape proposals protect.
    let exact_single = shared_len == 1 && token_overlap >= 1.0;
    if (shared_len < 2 && !exact_single) || token_overlap < 0.60 {
        return None;
    }
    // An exact single-token overlap is an exact-name match: medium, like the
    // strong token-overlap band.
    let confidence = if token_overlap >= 0.80 {
        ReuseConfidence::Medium
    } else {
        ReuseConfidence::Low
    };
    Some((
        DetectorId::D2,
        (token_overlap * 100.0).round() as u64,
        confidence,
        vec![ReasonCode::ProposalTokenOnly],
    ))
}

fn proposal_language(signature: &str) -> Option<LangFamily> {
    let trimmed = signature.trim();
    if trimmed.split_whitespace().any(|tok| tok == "def")
        || (trimmed.contains("class ") && trimmed.ends_with(':') && !trimmed.contains('{'))
    {
        Some(LangFamily::Python)
    } else if trimmed.split_whitespace().any(|tok| tok == "fn") {
        Some(LangFamily::Rust)
    } else if trimmed.contains("function ")
        || trimmed.contains("function*")
        || trimmed.contains("=>")
        || trimmed.contains("export class ")
        || trimmed.contains("export const ")
        || trimmed.contains("export let ")
        || trimmed.contains("export default ")
        || trimmed.contains("export async ")
        || (trimmed.contains("class ") && trimmed.contains('{'))
    {
        Some(LangFamily::TsJs)
    } else {
        None
    }
}

fn proposal_name(signature: &str) -> Option<String> {
    let trimmed = signature.trim();
    let mut words = trimmed.split_whitespace();
    while let Some(word) = words.next() {
        if matches!(
            word,
            "def" | "fn" | "function" | "function*" | "class" | "method" | "const" | "let" | "var"
        ) {
            if let Some(target) = words.next() {
                let name = target
                    .trim_start_matches('*')
                    .split(['(', '<', ' ', ':', '='])
                    .next()
                    .unwrap_or("")
                    .trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    let name = trimmed
        .split(['(', '<', ' ', ':'])
        .next()
        .unwrap_or("")
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn callee_set(values: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for value in values {
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if is_raw_client(part) {
                out.insert(part.to_string());
                continue;
            }
            let normalized = part.rsplit(['.', ':']).next().unwrap_or(part);
            out.insert(normalized.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_parses_ts_js_export_class_arrow_and_generator() {
        assert_eq!(
            proposal_language("export class TokenStore {"),
            Some(LangFamily::TsJs)
        );
        assert_eq!(
            proposal_name("export class TokenStore {"),
            Some("TokenStore".to_string())
        );
        assert_eq!(
            proposal_language("export const parseToken = (raw: string) => raw.trim()"),
            Some(LangFamily::TsJs)
        );
        assert_eq!(
            proposal_name("export const parseToken = (raw: string) => raw.trim()"),
            Some("parseToken".to_string())
        );
        assert_eq!(
            proposal_language("export function* iterItems(items: string[]) {"),
            Some(LangFamily::TsJs)
        );
        assert_eq!(
            proposal_name("export function* iterItems(items: string[]) {"),
            Some("iterItems".to_string())
        );
        assert_eq!(
            proposal_language("class TokenStore:"),
            Some(LangFamily::Python)
        );
    }

    #[test]
    fn proposal_applies_stoplist_symmetrically_with_token_fallback() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../schema/index.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes)
             VALUES (1, 'a.py', 10, 1, 1, 'ok', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end, signature)
             VALUES (1, 'function', 'parse_widget_token', 'a.parse_widget_token', 1, 'a.py', 1, 3, '')",
            [],
        )
        .unwrap();
        let opts = ReuseOpts::new_proposal(
            "def parse_widget_token(raw):".to_string(),
            vec!["unresolved_external_call".to_string()],
            10,
            false,
            ReuseConfidence::Low,
            false,
        )
        .unwrap();
        let pool = analyze(&conn, &opts).unwrap();
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].canonical, "a.parse_widget_token");
        assert_eq!(pool[0].detector, DetectorId::D2);
    }
}
