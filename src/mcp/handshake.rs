//! MCP connection bootstrap: the initialize handshake, root resolution
//! (file URIs, percent-decoding, git-ancestor search), and the tools/call
//! argument gate. Protocol framing lives in the parent module.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{write_result, PROTOCOL_VERSION};

pub(super) fn handle_initialize(
    params: Option<&Value>,
    root: &mut Option<PathBuf>,
    id: Option<Value>,
    out: &mut impl Write,
) -> std::io::Result<()> {
    if let Some(p) = params {
        if let Some(client_name) = p
            .get("clientInfo")
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
        {
            crate::journal::set_agent_context(client_name);
        }
    }
    if root.is_none() {
        if let Some(p) = params {
            let root_candidate = p
                .get("rootUri")
                .and_then(|u| u.as_str())
                .or_else(|| p.get("rootPath").and_then(|u| u.as_str()))
                .or_else(|| {
                    p.get("workspaceFolders")
                        .and_then(|wf| wf.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|f| f.get("uri"))
                        .and_then(|u| u.as_str())
                });
            if let Some(uri) = root_candidate {
                // file URIs percent-encode paths with spaces (%20) and
                // non-ASCII; the raw string never matches the real directory.
                let path_str = percent_decode(uri.strip_prefix("file://").unwrap_or(uri));
                let path = Path::new(&path_str);
                if let Some(r) = root_from_path(path) {
                    *root = Some(r);
                }
            }
        }
    }
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_VERSION)
        .to_string();
    let result = json!({
        "protocolVersion": requested,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "engram", "version": env!("CARGO_PKG_VERSION")},
    });
    write_result(out, id, result)
}

/// Decode percent-encoded bytes in a file URI path. Invalid escapes pass
/// through untouched; output stays lossy UTF-8 like the rest of the surface.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(decoded) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(decoded);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Active-root fallback shared by initialize and tools/call, resolved through
/// the shared `RepoRef` resolver: a nested `.engram/` store wins over the
/// enclosing git root (matching the CLI), and a subdirectory resolves to its
/// repository instead of standing in for one.
pub(super) fn root_from_path(start: &Path) -> Option<PathBuf> {
    crate::repo::RepoRef::resolve(Some(start))
        .ok()
        .map(|r| r.root().to_path_buf())
}
