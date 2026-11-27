//! Shared `eg-judge` binary/model discovery and bounded subprocess execution.
//! Used by `memory::embed`, `memory::judge_hint`, and `query::reuse::judge`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Resolve the `eg-judge` binary: `EG_JUDGE_BIN` (must exist as a file) wins,
/// then `PATH` search.
pub(crate) fn find_bin() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("EG_JUDGE_BIN") {
        if !value.is_empty() {
            let path = PathBuf::from(value);
            return path.is_file().then_some(path);
        }
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join("eg-judge"))
        .find(|candidate| candidate.is_file())
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
