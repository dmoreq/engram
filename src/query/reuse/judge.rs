//! Phase 5 hardened judge. `eg reuse --judge` asks a local `eg-judge` binary
//! for a verdict on gray-zone candidates. Missing binaries, model failures,
//! timeouts, malformed output, and unknown verdicts never alter the static
//! result; they add one stderr note.
//!
//! The batch contract: one NDJSON payload, one child process, one checkpoint
//! load, one verdict line per pair in order.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::*;

pub(super) const GRAY_LOW: u64 = 45;
pub(super) const GRAY_HIGH: u64 = 75;
/// Batch ceiling: one NDJSON line per judged candidate.
const MAX_JUDGED: usize = 20;
/// Cold Metal kernel compilation measured at 13.1 s; the deadline must cover
/// one cold start plus a warm batch of 20 pairs.
const TIMEOUT: Duration = Duration::from_millis(20_000);
const POLL: Duration = Duration::from_millis(10);
const MAX_INPUT: usize = 256 * 1024;
const MAX_OUTPUT: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerdictDecision {
    Duplicate,
    Distinct,
}

struct Setup {
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
        });
        return Some(
            "eg: judge unavailable: judge payload exceeds the cap; static result kept".to_string(),
        );
    };
    let reviewed = pairs.len();
    let mut outcome = BatchOutcome {
        reviewed,
        rejected: 0,
        failed: reviewed,
        reject_indices: Vec::new(),
    };
    if let Some(lines) = run_child_batch(&setup, &payload) {
        if lines.len() == reviewed {
            outcome.failed = 0;
            for (position, raw) in lines.iter().enumerate() {
                match parse_verdict(raw) {
                    Some(VerdictDecision::Distinct) => {
                        outcome.reject_indices.push(indices[position]);
                        outcome.rejected += 1;
                    }
                    Some(VerdictDecision::Duplicate) => {}
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
    if outcome.failed > 0 {
        note.push_str(&format!(", {} failed", outcome.failed));
    }
    Some(note)
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
    let right = source(ctx, &candidate.canonical_file, &candidate.canonical)?;
    Some((left, right))
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

/// One NDJSON line per pair, in candidate order.
fn build_payload(setup: &Setup, pairs: &[(String, String)]) -> Option<Vec<u8>> {
    let mut payload = Vec::new();
    for (left, right) in pairs {
        let line = if setup.subcommand == "similarity" {
            serde_json::json!({"text_a": left, "text_b": right})
        } else {
            serde_json::json!({"code_a": left, "code_b": right})
        };
        serde_json::to_writer(&mut payload, &line).ok()?;
        payload.push(b'\n');
    }
    if payload.len() > MAX_INPUT {
        return None;
    }
    Some(payload)
}

/// Spawn one child, feed the batch, and return its verdict lines in order.
/// `None` means the whole batch failed (spawn, timeout, cap, or shape).
fn run_child_batch(setup: &Setup, payload: &[u8]) -> Option<Vec<String>> {
    let mut command = Command::new(&setup.binary);
    command
        .arg(setup.subcommand)
        .arg("--stdin")
        .arg("--model")
        .arg(&setup.model);
    if let Some(recipe) = &setup.recipe {
        command.arg("--recipe").arg(recipe);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();
    if let Some(home) = &setup.home {
        command.env("HOME", home);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().ok()?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let payload = payload.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                kill_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                return None;
            }
        }
    }
    // The judge may have exited after forking a helper that inherited stdout.
    // Tear down the process group before draining so the read is bounded.
    kill_group(&child);
    let _ = writer.join();
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut raw = String::new();
        let result = stdout
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_string(&mut raw)
            .ok()
            .map(|_| raw);
        let _ = tx.send(result);
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    let raw = match rx.recv_timeout(remaining.max(Duration::from_millis(1))) {
        Ok(Some(raw)) => raw,
        _ => return None,
    };
    if raw.len() > MAX_OUTPUT {
        return None;
    }
    let lines: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Some(lines)
}

#[cfg(unix)]
fn kill_group(child: &std::process::Child) {
    let group = format!("-{}", child.id());
    for signal in ["-TERM", "-KILL"] {
        let _ = Command::new("kill")
            .arg(signal)
            .arg(&group)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(not(unix))]
fn kill_group(_child: &std::process::Child) {}

fn parse_verdict(raw: &str) -> Option<VerdictDecision> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    let decision = match value.get("decision")?.as_str()? {
        "duplicate" => VerdictDecision::Duplicate,
        "distinct" => VerdictDecision::Distinct,
        _ => return None,
    };
    let probability = value
        .get("probability")
        .or_else(|| value.get("similarity"))?
        .as_f64()?;
    if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
        return None;
    }
    Some(decision)
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
