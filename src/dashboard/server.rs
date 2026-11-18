//! Native synchronous HTTP 1.1 and Server-Sent Events server in Rust stdlib.
//! Zero async runtimes, zero external web crates, zero Python dependency.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) mod http;
mod sse;
pub(crate) mod touch;

use crate::dashboard::data;
use crate::db;
use crate::error::{EgError, Result};
use http::{host_allowed, read_http_request, send_response, RequestLog};
pub use sse::poll_sse_file;
use sse::stream_sse_events;

static INDEX_HTML: &str = include_str!("assets/index.html");
static STYLE_CSS: &str = include_str!("assets/style.css");
static APP_JS: &str = include_str!("assets/app.js");
static FAVICON_SVG: &str = include_str!("assets/favicon.svg");
static LOGO_SVG: &str = include_str!("assets/logo.svg");
static LOGO_LIGHT_SVG: &str = include_str!("assets/logo-light.svg");

pub fn serve_dashboard(
    root: Option<&Path>,
    host: &str,
    port: u16,
    open: bool,
    local: bool,
) -> Result<()> {
    if !matches!(host, "127.0.0.1" | "localhost") {
        return Err(EgError::Usage(format!(
            "dashboard binds loopback only (got --host {host}); telemetry carries local paths"
        )));
    }
    let bind_addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind_addr).map_err(|e| {
        EgError::Internal(format!(
            "could not bind dashboard server to {bind_addr}: {e}"
        ))
    })?;

    let url = format!("http://{host}:{port}/");
    println!("eg: live dashboard running at {url}");
    println!("Press Ctrl+C to stop.");

    if open {
        open_browser(&url);
    }

    // `--local` must isolate one repository: resolve its root once, at
    // startup, instead of letting discovery scan every journaled repo.
    let root_path: Option<PathBuf> = if local && root.is_none() {
        match crate::walk::discover_root(None) {
            Ok(root) => Some(root),
            Err(e) => {
                return Err(EgError::Usage(format!(
                    "--local needs a resolvable repository root: {e}"
                )))
            }
        }
    } else {
        root.map(Path::to_path_buf)
    };

    let log_path = if local {
        let r = root_path
            .as_deref()
            .ok_or_else(|| EgError::Usage("--local needs a repository root".to_string()))?;
        // Local mode reads and displays this repo's journal; refuse a tracked
        // or symlinked confinement directory before opening any panel.
        crate::db::reject_symlink(&crate::db::eg_dir(r), ".engram directory")?;
        crate::db::reject_unsafe_journal(r)?;
        crate::db::reject_tracked_eg_dir(r)?;
        crate::journal::usage_log_path(r)
    } else {
        db::global_usage_log_path().unwrap_or_else(|_| PathBuf::from("usage.log"))
    };

    let mode = if local { "local" } else { "global" };
    eprintln!(
        "eg dashboard: mode={mode} root={} log={}",
        root_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string()),
        log_path.display()
    );

    let _ = crate::journal::prune_stale_entries_all(&log_path);
    if !local {
        // Global telemetry lists durable repositories: probe fixtures under
        // the OS temp tree are scratch identities, not projects.
        let _ = crate::journal::prune_ephemeral_entries_all(&log_path);
    }

    let running = Arc::new(AtomicBool::new(true));

    for stream in listener.incoming() {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        match stream {
            Ok(stream) => {
                admit_connection(stream, log_path.clone(), root_path.clone(), !local);
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    Ok(())
}

/// Admit one accepted connection or refuse it under the concurrency cap: a
/// refusal answers `503` and closes without spawning a thread, so a flood of
/// idle sockets cannot exhaust thread count or memory.
pub fn admit_connection(
    stream: TcpStream,
    log_path: PathBuf,
    root: Option<PathBuf>,
    is_global: bool,
) {
    // Reserve a slot atomically: a check-then-increment race could admit two
    // threads past a full cap.
    if ACTIVE_CONNS
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
            cur.checked_add(1).filter(|&c| c < MAX_CONCURRENT_CONNS)
        })
        .is_err()
    {
        log_connection_cap_reached(MAX_CONCURRENT_CONNS);
        let mut stream = stream;
        let _ = send_response(&mut stream, 503, "text/plain; charset=utf-8", b"", false);
        return;
    }
    thread::spawn(move || {
        let _guard = ConnGuard;
        let _ = handle_client(stream, &log_path, root.as_deref(), is_global);
    });
}

fn open_browser(url: &str) {
    let (cmd, args) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", url])
    } else {
        ("xdg-open", vec![url])
    };
    let _ = std::process::Command::new(cmd).args(&args).spawn();
}

/// Serve an embedded SVG brand asset with the shared response writer.
fn serve_svg(stream: &mut TcpStream, svg: &'static str, is_head: bool) -> std::io::Result<bool> {
    send_response(stream, 200, "image/svg+xml", svg.as_bytes(), is_head)?;
    Ok(true)
}

/// One parsed HTTP request. Header totals are bounded so a hostile local page
/// cannot hold a thread with an endless header block.
fn serve_static_asset(stream: &mut TcpStream, path: &str, is_head: bool) -> std::io::Result<bool> {
    match path {
        "/" | "/index.html" => {
            send_response(
                stream,
                200,
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
                is_head,
            )?;
            Ok(true)
        }
        "/style.css" => {
            send_response(
                stream,
                200,
                "text/css; charset=utf-8",
                STYLE_CSS.as_bytes(),
                is_head,
            )?;
            Ok(true)
        }
        "/app.js" => {
            send_response(
                stream,
                200,
                "text/javascript; charset=utf-8",
                APP_JS.as_bytes(),
                is_head,
            )?;
            Ok(true)
        }
        "/favicon.svg" | "/favicon.ico" => serve_svg(stream, FAVICON_SVG, is_head),
        "/logo.svg" => serve_svg(stream, LOGO_SVG, is_head),
        "/logo-light.svg" => serve_svg(stream, LOGO_LIGHT_SVG, is_head),
        _ => Ok(false),
    }
}

/// Journal entries for one panel request. Global mode hides scratch roots even
/// when the startup prune could not rewrite the log (read-only store, a probe
/// journaling while the server runs), so no table can list an ephemeral repo.
fn read_entries(log_path: &Path, global: bool) -> Vec<serde_json::Value> {
    let mut entries = data::read_usage_entries(log_path);
    if global {
        entries.retain(|entry| !crate::journal::liveness::entry_is_ephemeral(entry));
    }
    entries
}

fn send_json(
    stream: &mut TcpStream,
    value: &serde_json::Value,
    is_head: bool,
) -> std::io::Result<bool> {
    let encoded = serde_json::to_vec_pretty(value).unwrap_or_default();
    send_response(
        stream,
        200,
        "application/json; charset=utf-8",
        &encoded,
        is_head,
    )?;
    Ok(true)
}

fn serve_api_route(
    stream: &mut TcpStream,
    path: &str,
    log_path: &Path,
    root: Option<&Path>,
    global: bool,
    is_head: bool,
) -> std::io::Result<bool> {
    match path {
        "/api/summary" | "/api/metrics" => {
            let entries = read_entries(log_path, global);
            let summary = data::compute_summary(&entries);
            send_json(stream, &summary, is_head)
        }
        "/api/reuse" => {
            let entries = read_entries(log_path, global);
            let (candidates, summary, prewrite) = data::collect_reuse_candidates(root, &entries)
                .unwrap_or_else(|_| (Vec::new(), serde_json::json!({}), Vec::new()));
            let payload = serde_json::json!({
                "candidates": candidates,
                "summary": summary,
                "prewrite_searches": prewrite,
            });
            send_json(stream, &payload, is_head)
        }
        "/api/memories" => {
            let entries = read_entries(log_path, global);
            let payload = data::collect_memories(root, &entries).unwrap_or_else(|e| {
                serde_json::json!({
                    "schema_version": 2, "notes": [], "degraded": true,
                    "errors": [{ "store": "-", "stage": "collect", "message": e.to_string() }]
                })
            });
            send_json(stream, &payload, is_head)
        }
        _ => Ok(false),
    }
}
/// Gate the captured local log path. Global mode reads `~/.engram/usage.log`
/// and is not repository provenance.
fn local_gate(root: Option<&Path>, global: bool) -> Result<()> {
    if global {
        return Ok(());
    }
    if let Some(root) = root {
        crate::db::reject_symlink(&crate::db::eg_dir(root), ".engram directory")?;
        crate::db::reject_unsafe_journal(root)?;
        crate::db::reject_tracked_eg_dir(root)?;
    }
    Ok(())
}

pub fn handle_client(
    mut stream: TcpStream,
    log_path: &Path,
    root: Option<&Path>,
    global: bool,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let Some(req) = read_http_request(&mut stream)? else {
        return Ok(());
    };
    let method = req.method.as_str();
    let path = req.path.split('?').next().unwrap_or(&req.path);
    // One stderr line per request: the only way to see a stuck or failing
    // route on a console dashboard.
    let _log = RequestLog {
        method: method.to_string(),
        path: path.to_string(),
        started: Instant::now(),
    };
    if !host_allowed(req.host.as_deref()) {
        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(resp.as_bytes())?;
        return Ok(());
    }
    // Local mode is revalidated per request: a session can become tracked
    // after startup, and the captured log path must not keep serving it.
    if let Err(err) = local_gate(root, global) {
        let body = format!("{err}\n");
        send_response(
            &mut stream,
            503,
            "text/plain; charset=utf-8",
            body.as_bytes(),
            method == "HEAD",
        )?;
        return Ok(());
    }

    if method == "POST" && path == "/api/memory/touch" {
        return touch::handle_memory_touch(&mut stream, root, log_path, global, &req);
    }

    if method != "GET" && method != "HEAD" {
        let resp =
            "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(resp.as_bytes())?;
        return Ok(());
    }

    let is_head = method == "HEAD";

    if serve_static_asset(&mut stream, path, is_head)? {
        return Ok(());
    }

    if serve_api_route(&mut stream, path, log_path, root, global, is_head)? {
        return Ok(());
    }

    if path == "/events" {
        if SSE_CLIENTS.fetch_add(1, Ordering::SeqCst) >= MAX_SSE_CLIENTS {
            SSE_CLIENTS.fetch_sub(1, Ordering::SeqCst);
            let resp = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(resp.as_bytes())?;
            return Ok(());
        }
        let _guard = SseGuard;
        if is_head {
            let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
            stream.write_all(header.as_bytes())?;
            return stream.flush();
        }
        return stream_sse_events(&mut stream, log_path, root, global);
    }

    let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    stream.write_all(resp.as_bytes())
}

static SSE_CLIENTS: AtomicUsize = AtomicUsize::new(0);
const MAX_SSE_CLIENTS: usize = 8;

/// Releases the SSE slot when the connection thread ends.
struct SseGuard;

impl Drop for SseGuard {
    fn drop(&mut self) {
        SSE_CLIENTS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Live connection threads, capped so saturation stays visible instead of
/// silently degrading into unbounded thread spawn.
static ACTIVE_CONNS: AtomicUsize = AtomicUsize::new(0);
const MAX_CONCURRENT_CONNS: usize = 64;

/// Last second (UNIX epoch) a cap-reached line was printed, for rate limiting.
static LAST_CAP_LOG_SEC: AtomicU64 = AtomicU64::new(0);

/// Releases the concurrency slot when the connection thread ends.
struct ConnGuard;

impl Drop for ConnGuard {
    fn drop(&mut self) {
        ACTIVE_CONNS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One stderr line per second at most: a rejection flood must be visible on
/// the console without drowning it.
fn log_connection_cap_reached(cap: usize) {
    let sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if LAST_CAP_LOG_SEC.swap(sec, Ordering::SeqCst) != sec {
        eprintln!("eg dashboard: connection cap reached ({cap})");
    }
}
