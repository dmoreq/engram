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

use super::child::{kill_group, parse_verdict, TIMEOUT};
use super::Setup;

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
        kill_group(&self.child);
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
    let mut command = std::process::Command::new(&setup.binary);
    command.arg("serve").arg("--model").arg(&setup.model);
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
        writeln!(session.stdin, "{request}").ok()?;
        session.stdin.flush().ok()?;
        match session.verdicts.recv_timeout(VERDICT_TIMEOUT) {
            Ok(Some(line)) => {
                // An error verdict keeps the session; parse_verdict maps
                // it to a per-pair failure upstream.
                if parse_verdict(&line).is_none() && line.contains("\"decision\":\"error\"") {
                    return None;
                }
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
