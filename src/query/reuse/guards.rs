//! Candidate guards and deterministic canonical ordering shared by D1-D4.

use super::corpus::SymbolData;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum LangFamily {
    Python,
    TsJs,
    Rust,
    Other(u64),
}

/// GUARD-LANG: Python, TsJs, Rust, or a hash of the lowercase extension.
pub(super) fn language_family(file: &str) -> LangFamily {
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "py" | "pyi" => LangFamily::Python,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => LangFamily::TsJs,
        "rs" => LangFamily::Rust,
        other => {
            let hash = other.bytes().fold(0xcbf2_9ce4_8422_2325u64, |acc, b| {
                (acc ^ u64::from(b.to_ascii_lowercase())).wrapping_mul(0x0000_0100_0000_01b3)
            });
            LangFamily::Other(hash)
        }
    }
}

pub(super) fn cross_language(a: &SymbolData, b: &SymbolData) -> bool {
    a.lang_family != b.lang_family
}

/// Deterministic canonical ordering: more callers wins; ties use the
/// ascending (file, line, qualname) key.
pub(super) fn is_more_canonical(candidate: &SymbolData, target: &SymbolData) -> bool {
    if candidate.inbound_callers != target.inbound_callers {
        candidate.inbound_callers > target.inbound_callers
    } else {
        (
            candidate.file_path.as_str(),
            candidate.line_start,
            candidate.qualname.as_str(),
        ) < (
            target.file_path.as_str(),
            target.line_start,
            target.qualname.as_str(),
        )
    }
}

/// Numbered siblings share a stem and are a deliberate family, not twins.
pub(super) fn strip_trailing_digits(name: &str) -> &str {
    name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_')
}

/// Weighted containment of the smaller deep footprint in the larger one.
pub(super) fn weighted_containment(target: &SymbolData, candidate: &SymbolData) -> f64 {
    let target_total = target.deep_total;
    let candidate_total = candidate.deep_total;
    if target_total <= 0.0 || candidate_total <= 0.0 {
        return 0.0;
    }
    let target_shared: f64 = target
        .deep_weights
        .iter()
        .filter(|(id, _)| candidate.deep_weights.contains_key(*id))
        .map(|(_, weight)| weight)
        .sum();
    let candidate_shared: f64 = candidate
        .deep_weights
        .iter()
        .filter(|(id, _)| target.deep_weights.contains_key(*id))
        .map(|(_, weight)| weight)
        .sum();
    (target_shared / target_total).max(candidate_shared / candidate_total)
}

/// Weighted size balance: `min(total_t, total_c) / max(total_t, total_c)`.
pub(super) fn weighted_balance(target: &SymbolData, candidate: &SymbolData) -> f64 {
    let target_total = target.deep_total;
    let candidate_total = candidate.deep_total;
    if target_total <= 0.0 || candidate_total <= 0.0 {
        return 0.0;
    }
    target_total.min(candidate_total) / target_total.max(candidate_total)
}

/// Jaccard similarity given intersection and union counts.
pub(super) fn jaccard(intersection: usize, union: usize) -> f64 {
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}
