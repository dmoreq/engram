//! SSE event streaming for the dashboard: the tail-poll pump, one
//! non-blocking disconnect probe, and the keepalive loop.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::data;
use super::local_gate;

pub(crate) fn stream_sse_events(
    stream: &mut TcpStream,
    log_path: &Path,
    root: Option<&Path>,
    global: bool,
) -> std::io::Result<()> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    // Same-origin only: the stream carries repository paths and search terms.
    let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
    stream.write_all(header.as_bytes())?;
    stream.flush()?;

    let mut tail = data::Tail::new(log_path);
    let mut last_keepalive = Instant::now();
    let mut last_gate = Instant::now();

    loop {
        if client_disconnected(&mut *stream)? {
            return Ok(());
        }
        if last_gate.elapsed() >= Duration::from_secs(5) {
            if local_gate(root, global).is_err() {
                return Ok(());
            }
            last_gate = Instant::now();
        }
        let now = Instant::now();
        poll_sse_file(
            stream,
            &mut tail,
            log_path,
            &mut last_keepalive,
            now,
            global,
        )?;

        if now.duration_since(last_keepalive) >= Duration::from_secs(15) {
            stream.write_all(b": keepalive\n\n")?;
            stream.flush()?;
            last_keepalive = now;
        }

        thread::sleep(Duration::from_millis(500));
    }
}

/// Non-blocking disconnect probe for SSE loops: a peek-style read of one byte
/// distinguishes a half-closed client (EOF), a live stream (WouldBlock /
/// pipelined bytes), and a reset. The SSE stream is always blocking here, so
/// the blocking flag is restored unconditionally. Stdlib-only.
fn client_disconnected(stream: &mut TcpStream) -> std::io::Result<bool> {
    stream.set_nonblocking(true)?;
    let mut byte = [0u8; 1];
    let gone = match stream.read(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            false
        }
        Err(_) => true,
    };
    stream.set_nonblocking(false)?;
    Ok(gone)
}

/// Pump one tail poll to the SSE client. A shrink or rotation emits the named
/// `reset` event and no replayed frames. Global mode hides scratch roots; a
/// linked-worktree frame is stamped with its main repository either way.
pub fn poll_sse_file(
    stream: &mut TcpStream,
    tail: &mut data::Tail,
    log_path: &Path,
    last_keepalive: &mut Instant,
    now: Instant,
    global: bool,
) -> std::io::Result<()> {
    let mut frames: Vec<String> = Vec::new();
    let mut reset = false;
    tail.poll(log_path, |event| match event {
        data::TailEvent::Reset => reset = true,
        data::TailEvent::Line(line) => {
            if let Some(mut val) = data::live_event(line) {
                if global && crate::journal::liveness::entry_is_ephemeral(&val) {
                    return;
                }
                data::annotate_worktree_main(&mut val);
                if let Ok(payload) = serde_json::to_string(&val) {
                    frames.push(format!("data: {payload}\n\n"));
                }
            }
        }
    });
    if reset {
        // Named event: a pre-upgrade tab's default onmessage handler ignores
        // it instead of treating it as a journal row.
        stream.write_all(b"event: reset\ndata: {}\n\n")?;
        *last_keepalive = now;
    }
    for frame in frames {
        stream.write_all(frame.as_bytes())?;
        *last_keepalive = now;
    }
    stream.flush()
}
