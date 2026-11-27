//! Anti-reinvention detection engine (D1-D4).
//! Referencing RepoReuse (arXiv:2609.35357) and codebase-minimalism.
//! Deterministic structural duplicate detectors.

mod allowlist;
mod candidate;
mod corpus;
mod coverage;
mod detectors;
mod finalize;
mod freshness;
mod guards;
mod index;
mod judge;
mod opts;
mod proposal;
mod status;
mod target;
mod tokens;
mod types;

use std::cell::Cell;
use std::collections::HashSet;

use rusqlite::Connection;

use crate::error::{EgError, Result};
pub(crate) use crate::query::sanitize;
use crate::query::QueryCtx;

use corpus::{
    load_callees, load_deep_weights, load_symbols, precompute_d1_callee_ids, select_targets,
};
use detectors::{Corpus, DETECTORS};
use index::{CorpusIndex, INVERTED_INDEX_MIN_SYMBOLS};
use opts::ReuseOpts;

pub(crate) use detectors::is_raw_client;
pub use detectors::DetectorId;
use tokens::{build_name_stoplist, build_stoplist};
pub use types::{
    Freshness, FreshnessMode, ReasonCode, ReuseConfidence, ReuseCounts, ReuseMode, UnknownReason,
};

/// Analysis output before confidence filtering and display truncation.
pub(super) struct ReuseReport {
    pub(super) mode: ReuseMode,
    pub(super) pool: Vec<ReuseCandidate>,
    pub(super) counts: ReuseCounts,
    pub(super) freshness: freshness::FreshnessReport,
    pub(super) path_matches: usize,
    pub(super) stderr: String,
}

#[derive(Debug, Clone)]
pub struct ReuseCandidate {
    pub detector: DetectorId,
    pub symbol: String,
    pub file: String,
    pub line: i64,
    pub line_end: i64,
    pub canonical: String,
    pub canonical_file: String,
    pub canonical_line: i64,
    pub canonical_line_end: i64,
    pub canonical_callers: i64,
    pub overlap_pct: Option<u64>,
    pub shared_items: Vec<String>,
    pub raw_client: Option<String>,
    pub confidence: ReuseConfidence,
    pub reasons: Vec<ReasonCode>,
}

impl ReuseCandidate {
    pub fn hint_line(&self) -> String {
        // The caller tail every detector renders the same way.
        let tail = format!(
            "([callers={}] in {}:{})",
            self.canonical_callers,
            sanitize(&self.canonical_file),
            self.canonical_line
        );
        let symbol = sanitize(&self.symbol);
        let canonical = sanitize(&self.canonical);
        match self.detector {
            // D1/D2 share the overlap template and differ only in the noun.
            DetectorId::D1 | DetectorId::D2 => format!(
                "symbol '{}' shares {}% {} with existing '{}' {}",
                symbol,
                self.overlap_pct.unwrap_or(0),
                if self.detector == DetectorId::D1 {
                    "callee footprint"
                } else {
                    "interface token overlap"
                },
                canonical,
                tail
            ),
            DetectorId::D3 => format!(
                "symbol '{}' bypasses canonical client '{}' to call raw external client {} {}",
                symbol,
                canonical,
                sanitize(self.raw_client.as_deref().unwrap_or("unknown")),
                tail
            ),
            DetectorId::D4 => format!(
                "symbol '{}' is a pass-through wrapper around existing '{}' {}",
                symbol, canonical, tail
            ),
            DetectorId::D5 => format!(
                "class '{}' overlaps existing '{}' {}",
                symbol, canonical, tail
            ),
            DetectorId::D1L => format!(
                "symbol '{}' has an unresolved callee matching existing '{}' {}",
                symbol, canonical, tail
            ),
            DetectorId::DT => format!(
                "test helper '{}' overlaps production '{}' {}",
                symbol, canonical, tail
            ),
        }
    }

    pub fn format_text(&self, explain: bool) -> String {
        let mut line = format!(
            "{} [confidence={}]",
            self.hint_line(),
            self.confidence.as_str()
        );
        if explain {
            let mut reasons: Vec<&str> = self.reasons.iter().map(|r| r.as_str()).collect();
            reasons.sort_unstable();
            line.push_str(&format!(
                " detector={}; reasons=[{}]",
                self.detector.as_str(),
                reasons.join(",")
            ));
        }
        if !self.shared_items.is_empty() {
            let label = match self.detector {
                DetectorId::D1 => "shared callees",
                DetectorId::D2 => "shared tokens",
                DetectorId::D3 => "client bypass",
                DetectorId::D4 => "details",
                DetectorId::D5 => "class methods",
                DetectorId::D1L => "unresolved callees",
                DetectorId::DT => "shared test tokens",
            };
            let items: Vec<String> = self
                .shared_items
                .iter()
                .map(|item| sanitize(item))
                .collect();
            line.push_str(&format!("\n  {}: [{}]", label, items.join(", ")));
        }
        line
    }
    /// Ordering rank used by the CLI and dashboard lists. D3/D4 carry no
    /// measured overlap, but they keep the historical rank (90/100) so the
    /// default `--limit` still surfaces wrappers; only the displayed metric
    /// was removed.
    fn sort_rank(&self) -> u64 {
        self.overlap_pct.unwrap_or(match self.detector {
            DetectorId::D3 => 90,
            DetectorId::D4 => 100,
            DetectorId::D5 => 80,
            DetectorId::D1L => 70,
            DetectorId::DT => 60,
            DetectorId::D1 | DetectorId::D2 => 0,
        })
    }
}

/// Detect up to 3 non-failing advisory lines for modified symbols during incremental build.
pub fn detect_hints(ctx: &QueryCtx<'_>, new_names: &HashSet<String>) -> Result<Vec<String>> {
    if new_names.is_empty() {
        return Ok(Vec::new());
    }
    let opts = ReuseOpts::for_hints();
    let scope = status::ResolvedScope {
        kind: status::ScopeKind::Symbols,
        paths: Vec::new(),
        symbols: new_names.clone(),
        files: Vec::new(),
        path_matches: 0,
    };
    let report = analyze_corpus_with(ctx, &opts, &scope)?;
    Ok(report
        .pool
        .iter()
        .take(opts.limit)
        .map(|c| format!("eg: reuse-hint: {}", c.hint_line()))
        .collect())
}

/// Turn on the judge coprocess for this process: `eg mcp` calls this once at
/// startup so gray-band reuse tool calls reuse one `eg-judge serve` child
/// across the session instead of spawning per call. CLI one-shot runs never
/// enable it.
pub fn enable_judge_coprocess() {
    judge::enable_coprocess();
}

pub struct ReuseRunArgs {
    pub paths: Vec<String>,
    pub limit: usize,
    pub deep_callee: bool,
    pub min_confidence: ReuseConfidence,
    pub explain: bool,
    pub show_suppressed: bool,
    pub coverage: Vec<String>,
    pub judge: bool,
}

pub struct ProposalRunArgs {
    pub signature: String,
    pub callees: Vec<String>,
    pub limit: usize,
    pub deep_callee: bool,
    pub min_confidence: ReuseConfidence,
    pub explain: bool,
    pub show_suppressed: bool,
    pub judge: bool,
}

/// Run `eg reuse` across the whole repository or specified paths.
pub fn run_reuse(ctx: &QueryCtx<'_>, args: ReuseRunArgs) -> Result<crate::query::LineQuery> {
    let opts = ReuseOpts::from_run_args(args)?;
    let scope = target::resolve_scope(ctx, &opts.paths)?;
    let report = analyze_corpus_with(ctx, &opts, &scope)?;
    finalize::finalize(report, &opts)
}

/// Phase 2 proposal entry point. Paths are rejected by the caller; the repo
/// scope supplies the freshness checker for canonical files.
pub fn run_reuse_proposal(
    ctx: &QueryCtx<'_>,
    args: ProposalRunArgs,
) -> Result<crate::query::LineQuery> {
    let opts = ReuseOpts::from_proposal_args(args)?;
    let scope = target::resolve_scope(ctx, &[])?;
    let report = analyze_corpus_with(ctx, &opts, &scope)?;
    finalize::finalize(report, &opts)
}

/// Analysis entry point: load the corpus, run guards/detectors or the
/// proposal matcher, and attach per-pair freshness. Display truncation
/// belongs to `finalize`.
pub(super) fn analyze_corpus_with(
    ctx: &QueryCtx<'_>,
    opts: &ReuseOpts,
    scope: &status::ResolvedScope,
) -> Result<ReuseReport> {
    let (mut pool, _total, guarded_out) = if opts.mode == ReuseMode::Propose {
        (proposal::analyze(ctx.conn, opts)?, 0, 0)
    } else if opts.mode == ReuseMode::Coverage {
        (
            coverage::analyze(ctx.conn, &opts.coverage, scope.path_filter())?,
            0,
            0,
        )
    } else {
        analyze_corpus(
            ctx.conn,
            scope.path_filter(),
            scope.target_names(),
            crate::query::MAX_LIMIT,
            opts.deep_callee,
        )?
    };
    let allowed = if pool.is_empty() {
        HashSet::new()
    } else {
        allowlist::load(ctx)
    };
    let mut suppressed = 0usize;
    if !allowed.is_empty() {
        if opts.show_suppressed {
            suppressed = pool
                .iter()
                .filter(|c| allowlist::is_allowed(&allowed, c))
                .count();
        } else {
            pool.retain(|candidate| {
                if allowlist::is_allowed(&allowed, candidate) {
                    suppressed += 1;
                    false
                } else {
                    true
                }
            });
        }
    }
    // Evaluate freshness before judge::apply so stale/unknown canonicals are
    // demoted and never promoted by a semantic duplicate verdict.
    let freshness = if opts.freshness_mode == FreshnessMode::Full {
        freshness::evaluate(ctx, scope, &mut pool)?
    } else {
        freshness::FreshnessReport::default()
    };
    // Judge BEFORE the confidence gate. The judge removes gray-band candidates
    // the semantic side calls distinct and promotes surviving gray duplicates,
    // so a sub-threshold candidate CAN return: an early gate would drop
    // exactly the candidates the judge exists to arbitrate, and
    // `--min-confidence medium` used to yield "0 judgeable candidates".
    // Without --judge the call is a no-op, so the reorder costs nothing on
    // the default path. `for_hints()` forces min_confidence Low, so hints are
    // unaffected.
    let judge_note = judge::apply(
        ctx,
        &mut pool,
        opts.judge,
        opts.mode,
        opts.proposal_signature.as_deref(),
        &allowed,
    );
    let below_threshold = pool
        .iter()
        .filter(|c| c.confidence.rank() < opts.min_confidence.rank())
        .count();
    pool.retain(|c| c.confidence.rank() >= opts.min_confidence.rank());
    let total = pool.len() + below_threshold;
    let counts = ReuseCounts {
        guarded_out,
        below_threshold,
        total,
        suppressed,
        ..ReuseCounts::default()
    };
    Ok(ReuseReport {
        mode: opts.mode,
        pool,
        counts,
        freshness,
        path_matches: scope.path_matches,
        stderr: judge_note.map(|n| format!("{n}\n")).unwrap_or_default(),
    })
}

pub(crate) fn analyze_corpus(
    conn: &Connection,
    path_filter: Option<&[String]>,
    target_names: Option<&HashSet<String>>,
    limit: usize,
    deep_callee: bool,
) -> Result<(Vec<ReuseCandidate>, usize, usize)> {
    let mut symbols = load_symbols(conn, false)?;
    if symbols.len() < 2 {
        return Ok((Vec::new(), 0, 0));
    }

    let target_ids: Vec<i64> = select_targets(&symbols, path_filter, target_names)
        .into_iter()
        .map(|s| s.id)
        .collect();
    if target_ids.is_empty() {
        return Ok((Vec::new(), 0, 0));
    }

    let df_all_excl = load_callees(conn, &mut symbols)?;
    let stopset = build_stoplist(&symbols);
    let name_stopset = build_name_stoplist(&symbols);
    for sym in symbols.values_mut() {
        // `all_callees` stays unfiltered: D4 needs the true distinct-callee
        // count, and a stoplisted constructor must still disqualify a
        // pass-through wrapper.
        sym.resolved_callees
            .retain(|c| !stopset.contains(c.as_str()));
        sym.name_tokens
            .retain(|token| !name_stopset.contains(token));
    }
    precompute_d1_callee_ids(&mut symbols);
    if deep_callee {
        load_deep_weights(&mut symbols);
    }

    let mut targets: Vec<&corpus::SymbolData> = target_ids
        .into_iter()
        .filter_map(|id| symbols.get(&id))
        .collect();
    // Duplicate qualnames across files race for the same deduped pair; sort by
    // the full identity so the surviving target (and its freshness) is stable.
    targets.sort_by(|a, b| {
        a.qualname
            .cmp(&b.qualname)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });
    let index = (symbols.len() > INVERTED_INDEX_MIN_SYMBOLS && targets.len() >= 16)
        .then(|| CorpusIndex::build(&symbols, deep_callee));
    let corpus = Corpus {
        symbols: &symbols,
        index: index.as_ref(),
        df_all_excl: Some(&df_all_excl),
        deep_callee,
        guarded_out: Cell::new(0),
        deep_budget: Cell::new(detectors::MAX_DEEP_PAIRS),
    };
    let mut candidates = Vec::new();
    let mut seen_pairs = HashSet::new();

    for target in &targets {
        // Every detector weighs in; the strongest confidence wins. A first-hit
        // break silently hid a stronger verdict from a later detector whenever
        // an earlier one matched.
        let mut best: Option<ReuseCandidate> = None;
        for detect in DETECTORS {
            if let Some(c) = detect(target, &corpus) {
                best = Some(match best {
                    None => c,
                    Some(previous) if previous.confidence.rank() >= c.confidence.rank() => previous,
                    _ => c,
                });
            }
        }
        if let Some(c) = best {
            accept_candidate(c, &mut seen_pairs, &mut candidates);
        }
    }

    sort_candidates(&mut candidates);
    let total = candidates.len();
    let guarded_out = corpus.guarded_out.get();
    candidates.truncate(limit);
    Ok((candidates, total, guarded_out))
}

pub(super) fn sort_candidates(pool: &mut [ReuseCandidate]) {
    pool.sort_by(|a, b| {
        b.confidence
            .rank()
            .cmp(&a.confidence.rank())
            .then_with(|| b.sort_rank().cmp(&a.sort_rank()))
            .then_with(|| b.canonical_callers.cmp(&a.canonical_callers))
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.canonical.cmp(&b.canonical))
            .then_with(|| a.canonical_file.cmp(&b.canonical_file))
            .then_with(|| a.canonical_line.cmp(&b.canonical_line))
    });
}

/// Dedup the (symbol, canonical) pair and push. A repeated pair is dropped;
/// the target keeps its highest-confidence detector verdict.
fn accept_candidate(
    candidate: ReuseCandidate,
    seen_pairs: &mut HashSet<(String, String)>,
    out: &mut Vec<ReuseCandidate>,
) {
    if seen_pairs.insert((candidate.symbol.clone(), candidate.canonical.clone())) {
        out.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn sanitize_escapes_backslash_before_named_controls() {
        assert_eq!(sanitize(r"a\b"), r"a\\b");
        assert_eq!(sanitize("a\nb\tc"), r"a\nb\tc");
        assert_eq!(sanitize("a\u{1b}b\u{7f}"), r"a\x1bb\x7f");
        assert_eq!(sanitize("a\u{202e}b"), r"a\u{202e}b");
    }
}
