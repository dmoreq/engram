//! Phase 5 hardened judge. `eg reuse --judge` asks a local `eg-judge` binary
//! for a verdict on gray-zone candidates. Missing binaries, model failures,
//! timeouts, malformed output, and unknown verdicts never alter the static
//! result; they add one stderr note.
//!
//! The batch contract: one NDJSON payload, one child process, one checkpoint
//! load, one verdict line per pair in order.

use std::path::PathBuf;

use serde_json::Value;

use super::*;

mod child;
mod coprocess;

use child::{build_payload, parse_verdict, run_child_batch};

pub(super) const GRAY_LOW: u64 = 45;
pub(super) const GRAY_HIGH: u64 = 75;
/// Batch ceiling: one NDJSON line per judged candidate. Warm verdicts cost
/// ~60 ms, so 40 pairs still fit the timeout next to the one-time Metal cold
/// start.
const MAX_JUDGED: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VerdictDecision {
    Duplicate,
    Distinct,
}

pub(super) struct Setup {
    binary: PathBuf,
    model: PathBuf,
    subcommand: &'static str,
    recipe: Option<PathBuf>,
    /// Preserved across the judge child's `env_clear()` so checkpoint and
    /// cache lookups that rely on `$HOME` keep working.
    home: Option<String>,
}

/// Judge outcome for one invocation, in candidate order.
struct BatchOutcome {
    reviewed: usize,
    rejected: usize,
    promoted: usize,
    failed: usize,
    /// Candidate pool indices rejected as distinct.
    reject_indices: Vec<usize>,
}

pub(super) fn apply(
    ctx: &QueryCtx<'_>,
    pool: &mut Vec<ReuseCandidate>,
    enabled: bool,
    mode: ReuseMode,
    proposal_signature: Option<&str>,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let setup = match detect_setup() {
        Ok(setup) => setup,
        Err(note) => return Some(format!("eg: judge unavailable: {note}")),
    };
    let mut indices: Vec<usize> = Vec::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (index, candidate) in pool.iter().enumerate() {
        if pairs.len() >= MAX_JUDGED || !is_judgeable(candidate, mode) {
            continue;
        }
        // A candidate whose source cannot be read safely is not judgeable; the
        // batch continues without it instead of failing every verdict.
        if let Some(pair) = pair_texts(ctx, candidate, proposal_signature) {
            indices.push(index);
            pairs.push(pair);
        }
    }
    if pairs.is_empty() {
        crate::journal::set_judged(false);
        crate::journal::set_judge_counts(crate::journal::JudgeJournalCounts {
            reviewed: 0,
            rejected: 0,
            failed: 0,
            rejected_loc: 0,
        });
        return Some("eg: judge: 0 judgeable candidates".to_string());
    }
    // Keep the batch inside the stdin cap by dropping trailing pairs.
    let mut payload = build_payload(&setup, &pairs);
    while payload.is_none() && pairs.len() > 1 {
        pairs.pop();
        indices.pop();
        payload = build_payload(&setup, &pairs);
    }
    let Some(payload) = payload else {
        crate::journal::set_judged(false);
        crate::journal::set_judge_counts(crate::journal::JudgeJournalCounts {
            reviewed: 1,
            rejected: 0,
            failed: 1,
            rejected_loc: 0,
        });
        return Some(
            "eg: judge unavailable: judge payload exceeds the cap; static result kept".to_string(),
        );
    };
    // The MCP coprocess answers one request line per verdict line on a
    // long-lived child; any failure tears the session down and the one-shot
    // batch below keeps the result deterministic.
    let lines =
        coprocess::judge_batch(&setup, &pairs).or_else(|| run_child_batch(&setup, &payload));
    let reviewed = pairs.len();
    let mut outcome = BatchOutcome {
        reviewed,
        rejected: 0,
        promoted: 0,
        failed: reviewed,
        reject_indices: Vec::new(),
    };
    if let Some(lines) = &lines {
        if lines.len() == reviewed {
            outcome.failed = 0;
            for (position, raw) in lines.iter().enumerate() {
                match parse_verdict(raw) {
                    Some(VerdictDecision::Distinct) => {
                        outcome.reject_indices.push(indices[position]);
                        outcome.rejected += 1;
                    }
                    Some(VerdictDecision::Duplicate) => {
                        // A semantic duplicate outranks the static provisional
                        // band: promote it into the display so a judged gray
                        // candidate survives a higher --min-confidence.
                        let candidate = &mut pool[indices[position]];
                        if candidate.confidence.rank() < ReuseConfidence::Medium.rank() {
                            candidate.confidence = ReuseConfidence::Medium;
                            outcome.promoted += 1;
                        }
                    }
                    None => outcome.failed += 1,
                }
            }
        }
    }
    let successes = outcome.reviewed.saturating_sub(outcome.failed);
    crate::journal::set_judged(successes > 0);
    crate::journal::set_judge_counts(crate::journal::JudgeJournalCounts {
        reviewed: outcome.reviewed,
        rejected: outcome.rejected,
        failed: outcome.failed,
        rejected_loc: rejected_symbol_loc(ctx, pool, &outcome.reject_indices),
    });
    if successes == 0 {
        return Some("eg: judge unavailable: arbitration failed; static result kept".to_string());
    }
    let mut index = 0usize;
    pool.retain(|_| {
        let keep = !outcome.reject_indices.contains(&index);
        index += 1;
        keep
    });
    let mut note = format!(
        "eg: judge: reviewed {}, rejected {}",
        outcome.reviewed, outcome.rejected
    );
    if outcome.promoted > 0 {
        note.push_str(&format!(", promoted {}", outcome.promoted));
    }
    if outcome.failed > 0 {
        note.push_str(&format!(", {} failed", outcome.failed));
    }
    Some(note)
}

/// Process-lifetime switch for the MCP coprocess session; see
/// [`crate::query::reuse::enable_judge_coprocess`].
pub(super) fn enable_coprocess() {
    coprocess::enable();
}

fn is_judgeable(candidate: &ReuseCandidate, mode: ReuseMode) -> bool {
    if mode == ReuseMode::Propose {
        return candidate.overlap_pct.is_some();
    }
    candidate
        .overlap_pct
        .is_some_and(|pct| (GRAY_LOW..=GRAY_HIGH).contains(&pct))
}

fn detect_setup() -> std::result::Result<Setup, String> {
    let binary = match std::env::var("EG_JUDGE_BIN") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => which("eg-judge")
            .ok_or_else(|| "eg-judge binary not found; set EG_JUDGE_BIN".to_string())?,
    };
    let model = match std::env::var("EG_JUDGE_MODEL") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            let home = std::env::var("HOME")
                .map_err(|_| "HOME is unset; set EG_JUDGE_MODEL".to_string())?;
            PathBuf::from(home)
                .join(".engram")
                .join("judge")
                .join("model")
        }
    };
    if !model.join("lazy.json").exists() {
        return Err(format!(
            "no checkpoint at {}; set EG_JUDGE_MODEL",
            model.display()
        ));
    }
    let subcommand = match std::fs::read_to_string(model.join("lazy.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("head_layers").and_then(Value::as_u64))
    {
        Some(0) => "similarity",
        _ => "judge-duplicate",
    };
    // A calibration recipe next to the checkpoint is picked up automatically;
    // the caller never edits the command to use it.
    let recipe = model
        .join("recipe.json")
        .is_file()
        .then(|| model.join("recipe.json"));
    Ok(Setup {
        binary,
        model,
        subcommand,
        recipe,
        home: std::env::var("HOME").ok(),
    })
}

fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn pair_texts(
    ctx: &QueryCtx<'_>,
    candidate: &ReuseCandidate,
    proposal_signature: Option<&str>,
) -> Option<(String, String)> {
    let left = if candidate.file == "<proposal>" {
        proposal_signature?.to_string()
    } else {
        source(ctx, &candidate.file, &candidate.symbol)?
    };
    let canonical = source(ctx, &candidate.canonical_file, &candidate.canonical)?;
    // A proposal pair compares shapes: the proposal side is one signature
    // line, so a 100-line canonical body would drown the signal. Trim the
    // canonical side to its declaration; real symbol pairs keep full bodies.
    let right = if candidate.file == "<proposal>" {
        signature_excerpt(&canonical)
    } else {
        canonical
    };
    Some((left, right))
}

/// Trim a symbol body to its declaration: everything up to and including the
/// line that opens the body (`{`, `=>`, or ends with `;`), capped at four
/// lines. A multi-line parameter list survives; the body does not.
fn signature_excerpt(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines().take(4) {
        let trimmed = line.trim_end();
        let opens = trimmed.ends_with('{') || trimmed.ends_with("=>") || trimmed.ends_with(';');
        out.push(line);
        if opens || out.len() == 4 {
            break;
        }
    }
    out.join("\n")
}

/// Summed span of the rejected candidates' symbols: the redundant lines the
/// arbitration removed from the agent's view. Unresolvable spans contribute 0
/// rather than guessing.
fn rejected_symbol_loc(
    ctx: &QueryCtx<'_>,
    pool: &[ReuseCandidate],
    reject_indices: &[usize],
) -> usize {
    reject_indices
        .iter()
        .filter_map(|&i| pool.get(i))
        .filter_map(|c| {
            ctx.conn
                .query_row(
                    "SELECT line_end - line_start FROM symbols \
                     WHERE file_path = ?1 AND (name = ?2 OR qualname = ?2) \
                     ORDER BY line_start, qualname LIMIT 1",
                    rusqlite::params![c.file, c.symbol],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
                .map(|v| v.max(0) as usize)
        })
        .sum()
}

/// Source text of one symbol from its indexed span. The path must stay inside
/// the repository and may not be a symlink.
fn source(ctx: &QueryCtx<'_>, file: &str, name: &str) -> Option<String> {
    if !crate::query::staleness::safe_index_path(file) {
        return None;
    }
    let abs = ctx.root.join(file);
    let meta = std::fs::symlink_metadata(&abs).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return None;
    }
    if meta.len() > crate::walk::MAX_FILE_SIZE {
        return None;
    }
    let canon_root = std::fs::canonicalize(ctx.root).ok()?;
    let canon = std::fs::canonicalize(&abs).ok()?;
    if !canon.starts_with(&canon_root) {
        return None;
    }
    let (start, end): (i64, i64) = ctx
        .conn
        .query_row(
            "SELECT line_start, line_end FROM symbols \
             WHERE file_path = ?1 AND (name = ?2 OR qualname = ?2) \
             ORDER BY line_start, qualname LIMIT 1",
            rusqlite::params![file, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    let text = std::fs::read_to_string(&canon).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let start = start.max(1) as usize - 1;
    let end = end.max(start as i64 + 1) as usize;
    if start >= lines.len() {
        return None;
    }
    Some(lines[start..end.min(lines.len())].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_band_is_inclusive() {
        let candidate = |pct| ReuseCandidate {
            detector: DetectorId::D1,
            symbol: "a".into(),
            file: "a.py".into(),
            line: 1,
            canonical: "b".into(),
            canonical_file: "b.py".into(),
            canonical_line: 1,
            canonical_callers: 0,
            overlap_pct: Some(pct),
            shared_items: Vec::new(),
            raw_client: None,
            confidence: ReuseConfidence::Low,
            reasons: Vec::new(),
        };
        assert!(!is_judgeable(&candidate(44), ReuseMode::Scan));
        assert!(is_judgeable(&candidate(45), ReuseMode::Scan));
        assert!(is_judgeable(&candidate(75), ReuseMode::Scan));
        assert!(!is_judgeable(&candidate(76), ReuseMode::Scan));
        assert!(is_judgeable(&candidate(100), ReuseMode::Propose));
    }

    #[test]
    fn verdict_whitelist_and_probability_range() {
        assert_eq!(
            parse_verdict(r#"{"decision":"duplicate","probability":0.8}"#),
            Some(VerdictDecision::Duplicate)
        );
        assert_eq!(
            parse_verdict(r#"{"decision":"distinct","similarity":0.2}"#),
            Some(VerdictDecision::Distinct)
        );
        assert_eq!(
            parse_verdict(r#"{"decision":"maybe","probability":0.8}"#),
            None
        );
        assert_eq!(
            parse_verdict(r#"{"decision":"duplicate","probability":1.2}"#),
            None
        );
    }

    #[test]
    fn batch_payload_uses_the_kind_specific_shape() {
        let setup = |subcommand: &'static str| Setup {
            binary: PathBuf::from("eg-judge"),
            model: PathBuf::from("model"),
            subcommand,
            recipe: None,
            home: None,
        };
        let pairs = vec![("left".to_string(), "right".to_string())];
        let payload = build_payload(&setup("similarity"), &pairs).unwrap();
        let text = String::from_utf8(payload).unwrap();
        assert!(text.contains("\"text_a\":\"left\""), "{text}");
        let payload = build_payload(&setup("judge-duplicate"), &pairs).unwrap();
        let text = String::from_utf8(payload).unwrap();
        assert!(text.contains("\"code_a\":\"left\""), "{text}");
    }
}
