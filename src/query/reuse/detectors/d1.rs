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

/// Weighted deep scan. Computes the expensive shared-evidence set only for a
/// candidate that can still beat the current best, and stops after the
/// per-invocation pair budget.
fn find_best_d1_deep(target: &SymbolData, corpus: &Corpus<'_>) -> Option<ReuseCandidate> {
    type DeepBest<'a> = (
        u64,
        &'a SymbolData,
        Vec<String>,
        ReuseConfidence,
        Vec<ReasonCode>,
    );
    let mut candidates = candidate_symbols(corpus, target, PostingKind::DeepCallee, 2);
    // Deterministic order before the budget-bounded loop so hitting
    // MAX_DEEP_PAIRS never depends on HashMap iteration order.
    candidates.sort_unstable_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.line_start.cmp(&b.line_start))
            .then_with(|| a.qualname.cmp(&b.qualname))
    });
    let mut best: Option<DeepBest<'_>> = None;
    for candidate in candidates {
        let same_file_overlap = candidate.file_id == target.file_id
            && !(candidate.line_end < target.line_start || candidate.line_start > target.line_end);
        if candidate.id == target.id || same_file_overlap {
            corpus.guard();
            continue;
        }
        if cross_language(target, candidate) || !is_more_canonical(candidate, target) {
            corpus.guard();
            continue;
        }
        if target.callee_ids.contains(&candidate.id)
            || candidate.callee_ids.contains(&target.id)
            || target.deep_weights.contains_key(&candidate.id)
            || candidate.deep_weights.contains_key(&target.id)
        {
            corpus.guard();
            continue;
        }
        if candidate.deep_weights.len() < 3 || candidate.deep_weights.len() > MAX_DEEP_FOOTPRINT {
            continue;
        }
        if corpus.deep_budget.get() == 0 {
            corpus.guard();
            break;
        }
        corpus.deep_budget.set(corpus.deep_budget.get() - 1);
        let ratio = weighted_containment(target, candidate);
        if ratio < 0.85 {
            continue;
        }
        let bal = weighted_balance(target, candidate);
        if bal < 0.30 {
            corpus.guard();
            continue;
        }
        let pct = (ratio * 100.0).round() as u64;
        let shared_ids: Vec<i64> = target
            .deep_weights
            .keys()
            .filter(|id| {
                // A cycle can place each pair member in the other's closure;
                // the pair itself is never evidence of its own duplication.
                **id != target.id && **id != candidate.id
            })
            .filter(|id| candidate.deep_weights.contains_key(*id))
            .copied()
            .collect();
        let mut shared: Vec<String> = shared_ids
            .iter()
            .filter_map(|id| corpus.symbols.get(id))
            .map(|sym| sym.name.clone())
            .collect();
        shared.sort();
        shared.dedup();
        let mut reasons = Vec::new();
        let unresolved_composition = target.unresolved_callees.contains(&candidate.name)
            || candidate.unresolved_callees.contains(&target.name);
        if unresolved_composition {
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
        let weak = distinctive_weakness(corpus, &shared_ids);
        let confidence = score_d1_confidence(&reasons, weak, ratio, bal);
        // Confidence ranks before raw overlap; the pct pre-filter cannot be
        // applied because a lower-overlap candidate may carry a higher rank.
        let better = best
            .as_ref()
            .is_none_or(|(best_pct, best_sym, _, best_conf, _)| {
                confidence.rank() > best_conf.rank()
                    || (confidence.rank() == best_conf.rank()
                        && (pct > *best_pct
                            || (pct == *best_pct && is_more_canonical(candidate, best_sym))))
            });
        if better {
            best = Some((pct, candidate, shared, confidence, reasons));
        }
    }
    best.map(|(pct, candidate, shared, confidence, reasons)| {
        make_candidate(
            DetectorId::D1,
            target,
            candidate,
            Some(pct),
            &shared,
            MatchMeta {
                raw_client: None,
                confidence,
                reasons,
            },
        )
    })
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
        |candidate| {
            let c_ids = &candidate.d1_callee_ids;
            // GUARD-COMP: a resolved call edge in either direction is a
            // caller/callee composition, not duplication.
            if target.callee_ids.contains(&candidate.id)
                || candidate.callee_ids.contains(&target.id)
            {
                corpus.guard();
                return None;
            }
            // Only unresolved bare-name overlaps are composition hints; a
            // call resolved to a different project symbol is not this pair.
            let unresolved_composition = target.unresolved_callees.contains(&candidate.name)
                || candidate.unresolved_callees.contains(&target.name);
            let zero_name = target
                .name_tokens
                .intersection(&candidate.name_tokens)
                .next()
                .is_none();
            if c_ids.len() < 3 {
                return None;
            }
            let shared_ids: Vec<i64> = target_ids.intersection(c_ids).copied().collect();
            if shared_ids.len() < 2 {
                return None;
            }
            let mut shared: Vec<String> = shared_ids
                .iter()
                .filter_map(|id| corpus.symbols.get(id))
                .map(|sym| sym.name.clone())
                .collect();
            shared.sort();
            shared.dedup();
            // GUARD-BAL: reject < 0.30, low in [0.30, 0.50), provisional >= 0.50.
            let min_len = target_ids.len().min(c_ids.len());
            let max_len = target_ids.len().max(c_ids.len());
            let bal = min_len as f64 / max_len as f64;
            if bal < 0.30 {
                corpus.guard();
                return None;
            }
            let ratio_candidate = shared_ids.len() as f64 / c_ids.len() as f64;
            let ratio_target = shared_ids.len() as f64 / target_ids.len() as f64;
            let ratio = ratio_candidate.max(ratio_target);
            if ratio < 0.80 {
                return None;
            }
            let pct = (ratio * 100.0).round() as u64;
            let mut reasons = Vec::new();
            if unresolved_composition {
                reasons.push(ReasonCode::CallerCallee);
            }
            if zero_name {
                reasons.push(ReasonCode::ZeroName);
            }
            if bal < 0.50 {
                reasons.push(ReasonCode::SizeImbalance);
            }
            let weak = distinctive_weakness(corpus, &shared_ids);
            let confidence = score_d1_confidence(&reasons, weak, ratio, bal);
            Some((pct, shared, confidence, reasons))
        },
    )
}
