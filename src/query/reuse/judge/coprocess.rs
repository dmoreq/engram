//! Persistent judge coprocess for the MCP server path. The session holds one
//! `eg-judge serve` child whose lifetime is bound to the server process: the
//! checkpoint loads once and every gray-band call answers in place instead of
//! paying a spawn plus Metal warm-up per tool call. CLI one-shot runs never
//! enable the slot, so `eg reuse --judge` keeps its one-shot batch.

use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};

use super::child::TIMEOUT;
use super::Setup;
use crate::memory::judge_proc;

/// Per-verdict deadline. The first request after a spawn may pay the full
/// Metal cold start, so this matches the one-shot batch deadline; warm
/// verdicts answer in ~60 ms.
const VERDICT_TIMEOUT: std::time::Duration = TIMEOUT;

/// Server-lifetime flag: only the MCP server turns this on.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// The slot plus a sticky flag for binaries without `serve`: one failed
/// first-request session stops further attempts instead of doubling every
/// call's latency.
static SLOT: OnceLock<Mutex<Option<SessionState>>> = OnceLock::new();

struct SessionState {
    session: Option<Session>,
    serve_unsupported: bool,
}

/// Session state: the child, its stdin writer, and the verdict reader
/// thread's channel. Dropping kills the process group, so the coprocess
/// never outlives the server.
struct Session {
    child: Child,
    stdin: ChildStdin,
    verdicts: Receiver<Option<String>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        judge_proc::kill_group(&self.child);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn slot() -> &'static Mutex<Option<SessionState>> {
    SLOT.get_or_init(|| {
        Mutex::new(Some(SessionState {
            session: None,
            serve_unsupported: false,
        }))
    })
}

/// Enable coprocess mode for this process (called by `eg mcp` only).
pub(super) fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

fn spawn(setup: &Setup) -> Option<Session> {
    // Head checkpoints classify through judge-duplicate questions, which
    // the line protocol does not carry; they keep the one-shot batch.
    if setup.subcommand != "similarity" {
        return None;
    }
    let mut command = judge_proc::base_command(&setup.binary, setup.home.as_deref());
    command.arg("serve").arg("--model").arg(&setup.model);
    if let Some(recipe) = &setup.recipe {
        command.arg("--recipe").arg(recipe);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(Some(line.trim().to_string())).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Some(Session {
        child,
        stdin,
        verdicts: rx,
    })
}

/// One request line per pair, one verdict line per pair. Returns `None`
/// when the session cannot answer (tears itself down; the caller falls
/// back to the one-shot batch).
pub(super) fn judge_batch(setup: &Setup, pairs: &[(String, String)]) -> Option<Vec<String>> {
    if !ENABLED.load(Ordering::SeqCst) || pairs.is_empty() {
        return None;
    }
    let mut guard = slot().lock().ok()?;
    let state = guard.as_mut()?;
    if state.serve_unsupported {
        return None;
    }
    let mut fresh = false;
    if state.session.is_none() {
        state.session = spawn(setup);
        fresh = true;
        if state.session.is_none() {
            state.serve_unsupported = true;
            return None;
        }
    }
    let session = state.session.as_mut()?;
    let mut lines = Vec::with_capacity(pairs.len());
    for (left, right) in pairs {
        let request = serde_json::json!({"text_a": left, "text_b": right});
        if writeln!(session.stdin, "{request}").is_err() || session.stdin.flush().is_err() {
            state.session = None;
            if fresh && lines.is_empty() {
                state.serve_unsupported = true;
            }
            return None;
        }
        match session.verdicts.recv_timeout(VERDICT_TIMEOUT) {
            Ok(Some(line)) => {
                // An error verdict keeps the session; parse_verdict maps
                // it to a per-pair failure upstream.
                lines.push(line);
            }
            // Child died or hung past the deadline: the session is toast.
            // A child that dies on its FIRST request cannot serve at all
            // (an old binary without the subcommand exits immediately),
            // so that shape is sticky for the server's lifetime; a later
            // death may be transient and is retried on the next call.
            Ok(None) | Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                state.session = None;
                if fresh && lines.is_empty() {
                    state.serve_unsupported = true;
                }
                return None;
            }
        }
    }
    Some(lines)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::db::test_support::{remove_tree, temp_root, write_file};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    #[test]
    fn dead_session_on_write_failure_recovers_on_next_call() {
        let root = temp_root("judge-coproc-recover");
        let script = root.join("eg-judge");
        // First invocation answers one line then exits immediately (so the 2nd
        // request hits broken pipe on stdin). Second invocation stays alive
        // and answers both requests.
        let marker = root.join("seen");
        let body = format!(
            "#!/bin/sh\nif [ ! -f '{}' ]; then\n  touch '{}'\n  IFS= read -r _\n  printf '{{\"decision\":\"duplicate\",\"similarity\":0.9}}\\n'\n  exit 0\nfi\nwhile IFS= read -r _; do\n  printf '{{\"decision\":\"distinct\",\"similarity\":0.1}}\\n'\ndone\n",
            marker.display(),
            marker.display()
        );
        write_file(&script, body.as_bytes()).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        enable();
        let setup = Setup {
            binary: script,
            model: PathBuf::from("model"),
            subcommand: "similarity",
            recipe: None,
            home: std::env::var("HOME").ok(),
        };
        let pairs = vec![
            ("a".to_string(), "b".to_string()),
            ("c".to_string(), "d".to_string()),
        ];
        // First call dies mid-batch (or on 2nd write) and must clear state.session.
        // Give the child a moment to exit after its 1st line if needed.
        let first = judge_batch(&setup, &pairs);
        assert!(first.is_none(), "mid-batch death should return None");
        // Next call must respawn a fresh session instead of staying wedged.
        let second = judge_batch(&setup, &pairs).expect("recovered session must answer");
        assert_eq!(second.len(), 2);
        remove_tree(&root);
    }
}
