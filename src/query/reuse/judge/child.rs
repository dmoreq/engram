//! Judge child-process transport: the NDJSON batch payload, the one-shot
//! spawn with its deadline and process-group teardown, and the verdict
//! parser. Pure process plumbing; no fs write primitives.

use std::time::Duration;

use serde_json::Value;

use super::{Setup, VerdictDecision};
use crate::memory::judge_proc;

/// Judge latency guard: one cold Metal start plus a warm batch of pairs, the
/// same deadline the coprocess applies per verdict.
pub(super) const TIMEOUT: Duration = Duration::from_millis(20_000);
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
    let mut command = judge_proc::base_command(&setup.binary, setup.home.as_deref());
    command
        .arg(setup.subcommand)
        .arg("--stdin")
        .arg("--model")
        .arg(&setup.model);
    if let Some(recipe) = &setup.recipe {
        command.arg("--recipe").arg(recipe);
    }
    let raw = judge_proc::run_bounded(command, Some(payload), TIMEOUT, MAX_OUTPUT)?;
    let lines: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Some(lines)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::db::test_support::{remove_tree, temp_root, write_file};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    #[test]
    fn child_stdout_over_16kib_does_not_deadlock() {
        let root = temp_root("judge-child-16k");
        let script = root.join("eg-judge");
        // Drain stdin with POSIX sh builtins (works under env_clear()) and emit
        // >32 KiB of NDJSON lines before exiting.
        let body = b"#!/bin/sh\nwhile IFS= read -r _; do :; done\ni=0\nwhile [ $i -lt 700 ]; do\n  printf '{\"decision\":\"duplicate\",\"probability\":0.9,\"pad\":\"abcdefghijklmnopqrstuvwxyz0123456789\"}\\n'\n  i=$((i + 1))\ndone\n";
        write_file(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let setup = Setup {
            binary: script,
            model: PathBuf::from("model"),
            subcommand: "similarity",
            recipe: None,
            home: std::env::var("HOME").ok(),
        };
        let lines = run_child_batch(&setup, b"{}\n").expect("must not deadlock on >16 KiB stdout");
        assert_eq!(lines.len(), 700);
        remove_tree(&root);
    }
}

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
