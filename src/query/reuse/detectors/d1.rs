//! D1 callee-footprint overlap. Counts project symbol IDs, never bare names,
//! so same-named helpers in different modules stay distinct.

use super::*;

/// D1's non-distinctive share: shared project callees called by more than two
/// sources. A pair whose shared footprint is mostly generic is Low even when
/// containment is high.
fn distinctive_weakness(corpus: &Corpus<'_>, shared_ids: &[i64]) -> f64 {
    if shared_ids.is_empty() {
        return 1.0;
    }
    let distinctive = shared_ids
        .iter()
        .filter(|id| {
            corpus
                .df_all_excl
                .is_none_or(|df| df.get(id).copied().unwrap_or(0) <= 2)
        })
        .count();
    (shared_ids.len().saturating_sub(distinctive)) as f64 / shared_ids.len() as f64
}

fn score_d1_confidence(reasons: &[ReasonCode], weak: f64, ratio: f64, bal: f64) -> ReuseConfidence {
    if !reasons.is_empty() || weak > 0.75 {
        ReuseConfidence::Low
    } else if ratio >= 0.95 && bal >= 0.75 && weak <= 0.25 {
        ReuseConfidence::High
    } else if ratio >= 0.90 {
        ReuseConfidence::Medium
    } else {
        ReuseConfidence::Provisional
    }
}

/// A two-hop closure this broad describes a hub, not a wrapper. Skipping it
/// keeps `--deep-callee` linear on generated hub corpora instead of comparing
/// every worker against every other worker's thousands of leaf callees.
const MAX_DEEP_FOOTPRINT: usize = 512;

fn score_d1_match(
    target: &SymbolData,
    candidate: &SymbolData,
    corpus: &Corpus<'_>,
    shared_ids: &[i64],
    ratio: f64,
    bal: f64,
) -> (u64, Vec<String>, ReuseConfidence, Vec<ReasonCode>) {
    let mut shared: Vec<String> = shared_ids
        .iter()
        .filter_map(|id| corpus.symbols.get(id))
        .map(|sym| sym.name.clone())
        .collect();
    shared.sort();
    shared.dedup();
    let mut reasons = Vec::new();
    // Only unresolved bare-name overlaps are composition hints; a call
    // resolved to a different project symbol is not this pair.
    if target.unresolved_callees.contains(&candidate.name)
        || candidate.unresolved_callees.contains(&target.name)
    {
        reasons.push(ReasonCode::CallerCallee);
    }
    if target
        .name_tokens
        .intersection(&candidate.name_tokens)
        .next()
        .is_none()
    {
        reasons.push(ReasonCode::ZeroName);
    }
    if bal < 0.50 {
        reasons.push(ReasonCode::SizeImbalance);
    }
    let weak = distinctive_weakness(corpus, shared_ids);
    let confidence = score_d1_confidence(&reasons, weak, ratio, bal);
    ((ratio * 100.0).round() as u64, shared, confidence, reasons)
}

/// Weighted deep scan. Stops after the per-invocation pair budget.
fn find_best_d1_deep(target: &SymbolData, corpus: &Corpus<'_>) -> Option<ReuseCandidate> {
    best_by_overlap(
        DetectorId::D1,
        target,
        corpus,
        PostingKind::DeepCallee,
        2,
        true,
        |candidate| {
            if target.callee_ids.contains(&candidate.id)
                || candidate.callee_ids.contains(&target.id)
                || target.deep_weights.contains_key(&candidate.id)
                || candidate.deep_weights.contains_key(&target.id)
            {
                corpus.guard();
                return ControlFlow::Continue(None);
            }
            if candidate.deep_weights.len() < 3 || candidate.deep_weights.len() > MAX_DEEP_FOOTPRINT
            {
                return ControlFlow::Continue(None);
            }
            if corpus.deep_budget.get() == 0 {
                corpus.guard();
                return ControlFlow::Break(());
            }
            corpus.deep_budget.set(corpus.deep_budget.get() - 1);
            let ratio = weighted_containment(target, candidate);
            if ratio < 0.85 {
                return ControlFlow::Continue(None);
            }
            let bal = weighted_balance(target, candidate);
            if bal < 0.30 {
                corpus.guard();
                return ControlFlow::Continue(None);
            }
            let shared_ids: Vec<i64> = target
                .deep_weights
                .keys()
                // A cycle can place each pair member in the other's closure;
                // the pair itself is never evidence of its own duplication.
                .filter(|id| **id != target.id && **id != candidate.id)
                .filter(|id| candidate.deep_weights.contains_key(*id))
                .copied()
                .collect();
            ControlFlow::Continue(Some(score_d1_match(
                target,
                candidate,
                corpus,
                &shared_ids,
                ratio,
                bal,
            )))
        },
    )
}

pub(crate) fn find_best_d1(target: &SymbolData, corpus: &Corpus<'_>) -> Option<ReuseCandidate> {
    if corpus.deep_callee {
        if target.deep_weights.len() < 3 || target.deep_weights.len() > MAX_DEEP_FOOTPRINT {
            corpus.guard();
            return None;
        }
        return find_best_d1_deep(target, corpus);
    }
    let target_ids = &target.d1_callee_ids;
    if target_ids.len() < 3 {
        return None;
    }
    best_by_overlap(
        DetectorId::D1,
        target,
        corpus,
        PostingKind::Callee,
        2,
        false,
        |candidate| {
            let c_ids = &candidate.d1_callee_ids;
            // GUARD-COMP: a resolved call edge in either direction is a
            // caller/callee composition, not duplication.
            if target.callee_ids.contains(&candidate.id)
                || candidate.callee_ids.contains(&target.id)
            {
                corpus.guard();
                return ControlFlow::Continue(None);
            }
            if c_ids.len() < 3 {
                return ControlFlow::Continue(None);
            }
            let shared_ids: Vec<i64> = target_ids.intersection(c_ids).copied().collect();
            if shared_ids.len() < 2 {
                return ControlFlow::Continue(None);
            }
            // GUARD-BAL: reject < 0.30, low in [0.30, 0.50), provisional >= 0.50.
            let min_len = target_ids.len().min(c_ids.len());
            let max_len = target_ids.len().max(c_ids.len());
            let bal = min_len as f64 / max_len as f64;
            if bal < 0.30 {
                corpus.guard();
                return ControlFlow::Continue(None);
            }
            let ratio = (shared_ids.len() as f64 / c_ids.len() as f64)
                .max(shared_ids.len() as f64 / target_ids.len() as f64);
            if ratio < 0.80 {
                return ControlFlow::Continue(None);
            }
            ControlFlow::Continue(Some(score_d1_match(
                target,
                candidate,
                corpus,
                &shared_ids,
                ratio,
                bal,
            )))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::deep_symbol;
    use super::*;
    use std::cell::Cell;

    #[test]
    fn deep_callee_budget_is_deterministic() {
        let picked: Vec<String> = (0..8)
            .map(|_| {
                let mut symbols = HashMap::new();
                symbols.insert(
                    10,
                    deep_symbol(10, "m.target", &["a", "b", "c"], &[], 10, 0),
                );
                symbols.insert(1, deep_symbol(1, "m.alpha", &["a", "b", "c"], &[], 1, 3));
                symbols.insert(2, deep_symbol(2, "m.beta", &["a", "b", "c"], &[], 2, 3));
                let corpus = Corpus {
                    symbols: &symbols,
                    index: None,
                    df_all_excl: None,
                    deep_callee: true,
                    guarded_out: Cell::new(0),
                    deep_budget: Cell::new(1),
                };
                find_best_d1(&symbols[&10], &corpus).unwrap().canonical
            })
            .collect();
        assert!(picked.iter().all(|c| c == "m.alpha"), "{picked:?}");
    }
}
