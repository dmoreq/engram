//! Judge child-process transport: the NDJSON batch payload, the one-shot
//! spawn with its deadline and process-group teardown, and the verdict
//! parser. Pure process plumbing; no fs write primitives.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Setup, VerdictDecision};

/// Judge latency guard: one cold Metal start plus a warm batch of pairs, the
/// same deadline the coprocess applies per verdict.
pub(super) const TIMEOUT: Duration = Duration::from_millis(20_000);
const POLL: Duration = Duration::from_millis(10);
const MAX_INPUT: usize = 256 * 1024;
pub(super) const MAX_OUTPUT: usize = 256 * 1024;

/// One NDJSON line per pair, in candidate order.
pub(super) fn build_payload(setup: &Setup, pairs: &[(String, String)]) -> Option<Vec<u8>> {
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
pub(super) fn run_child_batch(setup: &Setup, payload: &[u8]) -> Option<Vec<String>> {
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
pub(super) fn kill_group(child: &std::process::Child) {
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
pub(super) fn kill_group(_child: &std::process::Child) {}

/// The verdict whitelist: `duplicate`/`distinct` plus a finite probability in
/// [0, 1]. Anything else is a per-pair failure, never a panic.
pub(super) fn parse_verdict(raw: &str) -> Option<VerdictDecision> {
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
