//! Shared `eg-judge` binary/model discovery and bounded subprocess execution.
//! Used by `memory::embed`, `memory::judge_hint`, and `query::reuse::judge`.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Resolve the `eg-judge` binary: `EG_JUDGE_BIN` (must exist as a file) wins,
/// then `PATH` search, then `~/.cargo/bin/eg-judge`.
pub(crate) fn find_bin() -> Option<PathBuf> {
    find_bin_with(
        std::env::var("EG_JUDGE_BIN").ok().as_deref(),
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn find_bin_with(
    bin_env: Option<&str>,
    path_env: Option<&std::ffi::OsStr>,
    home_env: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if let Some(value) = bin_env {
        if !value.is_empty() {
            let path = PathBuf::from(value);
            return path.is_file().then_some(path);
        }
    }
    if let Some(paths) = path_env {
        if let Some(found) = std::env::split_paths(paths)
            .map(|dir| dir.join("eg-judge"))
            .find(|candidate| candidate.is_file())
        {
            return Some(found);
        }
    }
    let home = home_env?;
    let cargo_bin = PathBuf::from(home)
        .join(".cargo")
        .join("bin")
        .join("eg-judge");
    cargo_bin.is_file().then_some(cargo_bin)
}

/// Resolve the shared judge model directory (`EG_JUDGE_MODEL` or
/// `~/.engram/judge/model`) and verify it contains `lazy.json`.
pub(crate) fn resolve_model_dir() -> std::result::Result<PathBuf, String> {
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
    if !model.join("lazy.json").is_file() {
        return Err(format!(
            "no checkpoint at {}; set EG_JUDGE_MODEL",
            model.display()
        ));
    }
    Ok(model)
}

/// Build a sandboxed `Command` for `eg-judge` with `env_clear()`, preserving
/// `HOME` when provided and isolating the Unix process group.
pub(crate) fn base_command(binary: &Path, home: Option<&str>) -> Command {
    let mut command = Command::new(binary);
    command.env_clear();
    if let Some(home) = home {
        command.env("HOME", home);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// Tear down the child's process group on timeout or error so a forked helper
/// cannot hold pipes open.
#[cfg(unix)]
pub(crate) fn kill_group(child: &Child) {
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
pub(crate) fn kill_group(_child: &Child) {}

/// Execute a pre-configured `eg-judge` command with optional stdin payload,
/// bounded stdout capture, and a hard wall-clock timeout.
///
/// Spawns the bounded stdout reader thread before polling `try_wait()` so a
/// child writing >16 KiB to stdout cannot deadlock on a full OS pipe. Only
/// invokes `kill_group` on timeout, wait failure, or non-zero exit — never on
/// a clean exit.
pub(crate) fn run_bounded(
    mut command: Command,
    stdin_payload: Option<&[u8]>,
    timeout: Duration,
    max_output: usize,
) -> Option<String> {
    command
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = command.spawn().ok()?;
    let writer = if let Some(payload) = stdin_payload {
        let Some(mut stdin) = child.stdin.take() else {
            kill_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let owned = payload.to_vec();
        Some(std::thread::spawn(move || {
            let _ = stdin.write_all(&owned);
        }))
    } else {
        None
    };

    let Some(stdout) = child.stdout.take() else {
        kill_group(&child);
        let _ = child.kill();
        let _ = child.wait();
        if let Some(w) = writer {
            let _ = w.join();
        }
        return None;
    };

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut raw = String::new();
        let result = stdout
            .take((max_output + 1) as u64)
            .read_to_string(&mut raw)
            .ok()
            .map(|_| raw);
        let _ = tx.send(result);
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => {
                kill_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                if let Some(w) = writer {
                    let _ = w.join();
                }
                return None;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                kill_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                if let Some(w) = writer {
                    let _ = w.join();
                }
                return None;
            }
        }
    }

    if let Some(w) = writer {
        let _ = w.join();
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let raw = match rx.recv_timeout(remaining.max(Duration::from_millis(50))) {
        Ok(Some(raw)) => raw,
        _ => {
            kill_group(&child);
            return None;
        }
    };
    if raw.len() > max_output {
        return None;
    }
    Some(raw)
}

static COPROCESS_ENABLED: AtomicBool = AtomicBool::new(false);
static SERVE_SLOT: OnceLock<Mutex<Option<ServeState>>> = OnceLock::new();

struct ServeState {
    binary: PathBuf,
    model: PathBuf,
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<String>,
}

impl Drop for ServeState {
    fn drop(&mut self) {
        kill_group(&self.child);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Enable persistent `eg-judge serve` routing for `embed_text` and `judge_hint`.
#[allow(dead_code)]
pub(crate) fn enable_coprocess() {
    COPROCESS_ENABLED.store(true, Ordering::SeqCst);
}

pub(crate) fn is_coprocess_enabled() -> bool {
    COPROCESS_ENABLED.load(Ordering::SeqCst)
}

fn spawn_serve(binary: &Path, model: &Path, home: Option<&str>) -> Option<ServeState> {
    let mut command = base_command(binary, home);
    command
        .arg("serve")
        .arg("--model")
        .arg(model)
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
                    if tx.send(line.trim().to_string()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Some(ServeState {
        binary: binary.to_path_buf(),
        model: model.to_path_buf(),
        child,
        stdin,
        responses: rx,
    })
}

/// Send one JSON request line to a persistent `eg-judge serve` coprocess.
/// Returns `None` if coprocess mode is disabled or the session cannot answer,
/// letting callers fall back to one-shot `run_bounded`.
pub(crate) fn serve_request(
    binary: &Path,
    model: &Path,
    home: Option<&str>,
    request_line: &str,
    timeout: Duration,
) -> Option<String> {
    if !is_coprocess_enabled() {
        return None;
    }
    let cell = SERVE_SLOT.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().ok()?;
    let needs_spawn = guard
        .as_ref()
        .is_none_or(|s| s.binary != binary || s.model != model);
    if needs_spawn {
        *guard = spawn_serve(binary, model, home);
    }
    let state = guard.as_mut()?;
    if writeln!(state.stdin, "{request_line}").is_err() || state.stdin.flush().is_err() {
        *guard = None;
        return None;
    }
    match state.responses.recv_timeout(timeout) {
        Ok(line) if !line.is_empty() => Some(line),
        Ok(_) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
            *guard = None;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_bin_falls_back_to_cargo_bin_when_not_on_path() {
        let root = crate::db::test_support::temp_root("judge-find-bin");
        let empty_path = root.join("empty-path");
        crate::db::test_support::make_dir_all(&empty_path).unwrap();
        let cargo_bin = root.join(".cargo/bin/eg-judge");
        crate::db::test_support::make_dir_all(cargo_bin.parent().unwrap()).unwrap();
        crate::db::test_support::write_file(&cargo_bin, b"#!/bin/sh\n").unwrap();

        let found = find_bin_with(None, Some(empty_path.as_os_str()), Some(root.as_os_str()));
        assert_eq!(found.as_deref(), Some(cargo_bin.as_path()));

        // Empty EG_JUDGE_BIN also falls back instead of failing.
        let found_empty_env = find_bin_with(Some(""), None, Some(root.as_os_str()));
        assert_eq!(found_empty_env.as_deref(), Some(cargo_bin.as_path()));

        crate::db::test_support::remove_tree(&root);
    }
}
