//! Memory writes: note validation, normalization, revision outcome lines,
//! and local/global store resolution.

use super::*;
use crate::journal::JournalArgs;

/// Persist one note (validation, normalization, and the write outcome line).
pub(super) fn run_mem_write(
    req: &CommandRequest,
    root_arg: Option<&Path>,
    surface: Surface,
) -> Result<ArmOut> {
    let CommandRequest::MemWrite {
        key,
        body,
        kind,
        tags,
        refs,
        scope,
        ttl,
        supersedes,
        global,
        touch,
        confirm,
        expected_rev,
    } = req
    else {
        unreachable!("run_mem_write called with a non-write request")
    };
    if key.trim_start().starts_with("reuse-allow/") && *global {
        return Err(EgError::Usage(
            "mem write: reuse-allow notes must be local, not --global".to_string(),
        ));
    }
    if key.trim_start().starts_with("reuse-allow/") && surface == Surface::Mcp && !confirm {
        return Err(EgError::Usage(
            "mem write: MCP allowlist writes require confirm=true".to_string(),
        ));
    }
    if let Some(target) = supersedes {
        if target.trim() == key.trim() {
            return Err(EgError::Usage(
                "mem write: --supersedes must name a different key (same-key updates go through the revision chain)"
                    .to_string(),
            ));
        }
    }
    let parsed_scope = crate::memory::MemScope::parse(scope).ok_or_else(|| {
        EgError::Usage(format!(
            "mem write: scope must be 'durable' or 'session' (got '{scope}')"
        ))
    })?;
    let (root, index_conn) = if !*global {
        stale_ref_root_and_conn(root_arg, surface)
    } else {
        (None, None)
    };
    let hygiene_out = crate::memory::hygiene::validate_and_normalize_refs(
        refs.as_deref(),
        root.as_deref(),
        index_conn.as_deref(),
        parsed_scope,
        *global,
        *confirm,
    )?;
    let note = crate::memory::normalize_note(crate::memory::NoteInput {
        key,
        kind,
        tags: tags.as_deref(),
        refs: hygiene_out.refs.as_deref(),
        scope,
        body,
        ttl: *ttl,
    })?;
    let mut conn = open_mem_conn(root_arg, *global, MemoryMode::Store)?;
    // Advisory only: a near-duplicate under another key suggests --supersedes.
    // Same-key updates and explicit --supersedes already express intent.
    let consolidation = if supersedes.is_none() {
        crate::memory::find_consolidation_match(&conn, &note)
    } else {
        None
    };
    let outcome = write_note(
        &mut conn,
        &note,
        surface,
        supersedes.as_deref(),
        *touch,
        *expected_rev,
    )?;
    crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
    let line = outcome_line(&outcome, &note.key);
    let mut stdout = format!("{}\n{line}\n", crate::memory::MEMORY_CAVEAT);
    if hygiene_out.changed {
        stdout.push_str(&format!("normalized refs: {}\n", note.refs));
    }
    if let Some(m) = consolidation {
        let mut hint = m.hint_line(&note.key);
        if let Some(pct) = m.semantic_similarity_pct(&conn, &note) {
            hint.push_str(&format!(" (similarity={pct}%)"));
        }
        stdout.push_str(&hint);
        stdout.push('\n');
    }
    let mut stderr = String::new();
    for w in hygiene_out.warnings {
        stderr.push_str(&w);
        stderr.push('\n');
    }
    if !*global && !note.refs.is_empty() {
        for r in note.refs.split_whitespace() {
            let resolved =
                crate::db::resolve_ref_detail(index_conn.as_deref(), root.as_deref(), r, i64::MAX);
            if !resolved.status.is_verified() {
                stderr.push_str(&format!(
                    "eg: warning: unverified ref '{r}' (not found on disk or in index)\n"
                ));
            }
        }
    }
    Ok(ArmOut::plain(stdout, stderr))
}

fn write_note(
    conn: &mut rusqlite::Connection,
    note: &crate::memory::NormalizedNote,
    surface: Surface,
    supersedes: Option<&str>,
    touch: bool,
    expected_rev: Option<i64>,
) -> Result<crate::db::MemWriteOutcome> {
    let session = crate::journal::session_tag();
    let now = crate::db::unix_now();
    // Skip embedding when the active revision already has the same content
    // hash: `memory_write` will return `Duplicate` or `Touched` without
    // inserting a new row.
    let hash = crate::memory::mem_content_hash(
        note.kind.as_str(),
        &note.body,
        &note.tags,
        &note.refs,
        note.scope.as_str(),
    ) as i64;
    let skip_embed = conn
        .query_row(
            "SELECT content_hash, expires_at FROM memories
             WHERE key = ?1 AND superseded_by IS NULL
             ORDER BY rev DESC LIMIT 1",
            rusqlite::params![&note.key],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok()
        .is_some_and(|(existing_hash, expires_at)| {
            (expires_at == 0 || expires_at > now) && existing_hash == hash
        });
    let embedding = if skip_embed {
        None
    } else {
        crate::memory::embed::embed_text(&note.body)
            .map(|v| crate::memory::embed::embedding_blob(&v))
    };
    crate::db::memory_write(
        conn,
        &crate::db::MemNoteInput {
            key: &note.key,
            kind: note.kind.as_str(),
            tags: &note.tags,
            refs: &note.refs,
            scope: note.scope.as_str(),
            session: &session,
            body: &note.body,
            source: surface.as_str(),
            expires_at: if note.ttl_secs == 0 {
                0
            } else {
                now + note.ttl_secs
            },
            created_at: None,
            embedding: embedding.as_deref(),
            supersedes,
            touch,
            expected_rev,
        },
        now,
    )
}

fn outcome_line(outcome: &crate::db::MemWriteOutcome, key: &str) -> String {
    match outcome {
        crate::db::MemWriteOutcome::Written {
            rev,
            superseded_rev,
        } => match superseded_rev {
            Some(old) => format!("ok: '{key}' rev {rev} written (supersedes rev {old})"),
            None => format!("ok: '{key}' rev {rev} written"),
        },
        crate::db::MemWriteOutcome::Duplicate { rev } => {
            format!("ok: '{key}' unchanged at rev {rev} (identical content)")
        }
        crate::db::MemWriteOutcome::Touched { rev } => {
            format!("ok: '{key}' touched at rev {rev} (timestamp refreshed)")
        }
    }
}

/// The query-shaped fields of `MemQuery`, bundled once and passed to the
/// single-store and merged-store listings (the Data Clump those functions used
/// to receive as loose parameters; `include_superseded` rides in `filter`).
pub(super) struct MemQueryArgs<'a> {
    pub(super) terms: &'a [String],
    pub(super) all: bool,
    pub(super) limit: usize,
    pub(super) filter: crate::db::MemFilter,
    pub(super) index_conn: Option<&'a rusqlite::Connection>,
    pub(super) root: Option<&'a Path>,
    pub(super) reference: Option<&'a str>,
    pub(super) explain: bool,
    pub(super) legacy: bool,
}

/// Journal fields for `mem write` (key/kind/scope/ttl/supersedes/body_len —
/// never the body text).
pub(super) fn mem_write_args(req: &CommandRequest) -> JournalArgs {
    let CommandRequest::MemWrite {
        key,
        body,
        kind,
        scope,
        ttl,
        supersedes,
        global,
        touch,
        confirm,
        ..
    } = req
    else {
        unreachable!("mem_write_args called with a non-write request")
    };
    vec![
        ("key", key.clone()),
        ("kind", kind.clone()),
        ("scope", scope.clone()),
        ("ttl", ttl.map_or_else(String::new, |t| t.to_string())),
        ("supersedes", supersedes.clone().unwrap_or_default()),
        ("body_len", body.len().to_string()),
        ("global", global.to_string()),
        ("touch", touch.to_string()),
        ("confirm", confirm.to_string()),
    ]
}

/// Journal fields for `mem query` (terms/ref/kind/scope/limit/...).
pub(super) fn mem_query_args(req: &CommandRequest) -> JournalArgs {
    let CommandRequest::MemQuery {
        terms,
        all: _,
        kind,
        scope,
        limit,
        include_superseded,
        global,
        reference,
        since,
        until,
        local,
        explain,
        legacy,
    } = req
    else {
        unreachable!("mem_query_args called with a non-query request")
    };
    vec![
        ("terms", terms.join(" ")),
        ("ref", reference.clone().unwrap_or_default()),
        (
            "kind",
            kind.map_or_else(String::new, |k| k.as_str().to_string()),
        ),
        (
            "scope",
            scope.map_or_else(String::new, |s| s.as_str().to_string()),
        ),
        ("limit", limit.to_string()),
        ("include_superseded", include_superseded.to_string()),
        ("global", global.to_string()),
        ("since", since.map_or_else(String::new, |n| n.to_string())),
        ("until", until.map_or_else(String::new, |n| n.to_string())),
        ("local", local.to_string()),
        ("explain", explain.to_string()),
        ("legacy", legacy.to_string()),
    ]
}

/// The local and global memory stores a read command resolves once: `None`
/// means the store is absent (open errors other than "no store" propagate).
pub(super) struct MemoryStores {
    pub(super) local: Option<MemConn>,
    pub(super) global: Option<MemConn>,
}

/// Open the memory stores both `mem query` and `mem get` read: the local store
/// (only when root discovery and the open succeed) and the global one.
pub(super) fn resolve_memory_stores(
    root_arg: Option<&Path>,
    surface: Surface,
) -> Result<MemoryStores> {
    let local = match crate::walk::discover_root(root_arg) {
        Ok(root) => match open_mem_query_conn(Some(&root), false, surface) {
            Ok(conn) => Some(conn),
            Err(EgError::NoMemory(_)) => None,
            Err(e) => return Err(e),
        },
        // An explicit --root that cannot resolve is a usage error; an implicit
        // lookup with no git ancestor is not — the global store still applies.
        Err(e) if root_arg.is_some() => return Err(e),
        Err(_) => None,
    };
    let global = match open_mem_query_conn(None, true, surface) {
        Ok(conn) => Some(conn),
        Err(EgError::NoMemory(_)) => None,
        Err(e) => return Err(e),
    };
    Ok(MemoryStores { local, global })
}

/// The shared "no memory store anywhere" error: name the local root when
/// discovery worked, the generic write hint otherwise.
pub(super) fn no_memory_store_error(root_arg: Option<&Path>) -> EgError {
    if let Ok(root) = crate::walk::discover_root(root_arg) {
        crate::db::no_memory_error(&root)
    } else {
        EgError::NoMemory("no memory store found — write with eg mem write".to_string())
    }
}
