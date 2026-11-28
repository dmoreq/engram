//! Deterministic triage handler for `eg mem triage` and MCP `mem_triage`.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;

use super::*;
use crate::db::{IndexOpenError, RefStatus, ResolvedRef};
use crate::memory::triage::{
    classify_note_pure, render_triage_json, render_triage_text, BrokenRefEntry, GitStats,
    IndexState, SymbolCandidate, TriageItem, TriagePacket, TRIAGE_MAX_LIMIT,
    TRIAGE_MAX_SYMBOL_LOOKUPS,
};

struct RawNote {
    key: String,
    rev: i64,
    kind: String,
    scope: String,
    refs: String,
    created_at: i64,
    anchored_at: i64,
}

fn open_index_with_state(root: Option<&Path>) -> (Option<QueryConn>, IndexState) {
    let Some(r) = root else {
        return (None, IndexState::Missing);
    };
    match crate::db::try_open_index(r) {
        Ok(conn) => (Some(QueryConn::Owned(conn)), IndexState::Ok),
        Err(IndexOpenError::Missing) => (None, IndexState::Missing),
        Err(IndexOpenError::Corrupt) => (None, IndexState::Corrupt),
        Err(IndexOpenError::Schema { .. }) => (None, IndexState::SchemaSkew),
        Err(IndexOpenError::Symlink) => (None, IndexState::SecurityRejected),
    }
}

fn load_active_anchored_notes(
    conn: &Connection,
    key_filter: Option<&str>,
    after_key: Option<&str>,
    now: i64,
) -> Result<Vec<RawNote>> {
    let anchor_expr = if crate::db::column_present(conn, "memories", "anchored_at").unwrap_or(false)
    {
        "anchored_at"
    } else {
        "0"
    };
    let has_expiry = crate::db::column_present(conn, "memories", "expires_at").unwrap_or(false);
    let expiry_pred = if has_expiry {
        "AND (expires_at = 0 OR expires_at > ?1)"
    } else {
        ""
    };
    let key_pred = if key_filter.is_some() {
        "AND key = ?2"
    } else if after_key.is_some() {
        "AND key > ?2"
    } else {
        ""
    };
    let sql = format!(
        "SELECT key, rev, kind, scope, refs, created_at, {anchor_expr}
         FROM memories
         WHERE superseded_by IS NULL {expiry_pred} {key_pred}
         ORDER BY key ASC, rev DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(crate::db::map_memory_err)?;
    let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<RawNote> {
        Ok(RawNote {
            key: r.get(0)?,
            rev: r.get(1)?,
            kind: r.get(2)?,
            scope: r.get(3)?,
            refs: r.get(4)?,
            created_at: r.get(5)?,
            anchored_at: r.get(6)?,
        })
    };
    let rows = match (has_expiry, key_filter.or(after_key)) {
        (true, Some(k)) => stmt.query_map(rusqlite::params![now, k], map_row),
        (true, None) => stmt.query_map(rusqlite::params![now], map_row),
        (false, Some(k)) => stmt.query_map(rusqlite::params![0i64, k], map_row),
        (false, None) => stmt.query_map([], map_row),
    }
    .map_err(crate::db::map_memory_err)?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(crate::db::map_memory_err)?);
    }
    Ok(out)
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn load_local_recalls(root: Option<&Path>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    let Some(r) = root else {
        return counts;
    };
    let log_path = crate::journal::usage_log_path(r);
    let Ok(raw) = std::fs::read_to_string(log_path) else {
        return counts;
    };
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("tool").and_then(|x| x.as_str()) == Some("mem_get")
            && v.get("exit_code").and_then(|x| x.as_i64()) == Some(0)
        {
            if let Some(k) = v
                .get("args")
                .and_then(|a| a.get("key"))
                .and_then(|x| x.as_str())
            {
                *counts.entry(k.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn lookup_symbol_candidates(iconn: &Connection, token: &str) -> Vec<SymbolCandidate> {
    let clean = crate::db::split_line_suffix(token);
    let stem = Path::new(clean)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(clean);
    if stem.is_empty() {
        return Vec::new();
    }
    let pattern = format!("%{}%", like_escape(stem));
    let Ok(mut stmt) = iconn.prepare_cached(
        "SELECT qualname, file_path, line_start FROM symbols
         WHERE name = ?1 OR qualname LIKE ?2 ESCAPE '\\'
         ORDER BY qualname ASC, file_path ASC, line_start ASC LIMIT 5",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![stem, pattern], |r| {
        Ok(SymbolCandidate {
            qualname: r.get(0)?,
            file_path: r.get(1)?,
            line_start: r.get(2)?,
        })
    }) else {
        return Vec::new();
    };
    rows.filter_map(std::result::Result::ok).collect()
}

pub(super) fn run_mem_triage(
    req: &CommandRequest,
    root_arg: Option<&Path>,
    surface: Surface,
) -> Result<ArmOut> {
    let CommandRequest::MemTriage {
        key,
        after_key,
        limit,
        no_git,
        global,
        json,
    } = req
    else {
        unreachable!("run_mem_triage called with a non-triage request")
    };

    let limit = (*limit).clamp(1, TRIAGE_MAX_LIMIT);
    let now = crate::db::unix_now();
    let conn = open_mem_query_conn(root_arg, *global, surface)?;
    let root = if *global {
        None
    } else {
        root_arg
            .and_then(|r| crate::walk::discover_root(Some(r)).ok())
            .or_else(|| crate::walk::discover_root(None).ok())
    };
    let (index_conn, index_state) = open_index_with_state(root.as_deref());
    let canon_root = root.as_deref().and_then(|r| std::fs::canonicalize(r).ok());

    let raw_notes = load_active_anchored_notes(&conn, key.as_deref(), after_key.as_deref(), now)?;
    if let Some(k) = key {
        if raw_notes.is_empty() {
            return Err(mem_empty(&format!("unknown memory key '{k}'")));
        }
    }

    let recalls_map = if *global {
        HashMap::new()
    } else {
        load_local_recalls(root.as_deref())
    };
    let mut ref_cache: HashMap<String, ResolvedRef> = HashMap::new();
    let mut all_items: Vec<TriageItem> = Vec::new();
    for note in raw_notes {
        if note.refs.trim().is_empty() {
            continue;
        }
        let anchor = note.anchored_at.max(note.created_at);
        let mut resolved: Vec<(String, ResolvedRef)> = Vec::new();
        for tok in note.refs.split_whitespace() {
            let base = if let Some(cached) = ref_cache.get(tok) {
                cached.clone()
            } else {
                let rr = crate::db::resolve_ref_detail_with_canon(
                    index_conn.as_deref(),
                    root.as_deref(),
                    canon_root.as_deref(),
                    tok,
                    anchor,
                );
                ref_cache.insert(tok.to_string(), rr.clone());
                rr
            };
            resolved.push((tok.to_string(), base.with_anchor(anchor)));
        }

        let ref_status = if resolved.iter().any(|(_, r)| r.status == RefStatus::Broken) {
            "broken"
        } else if resolved
            .iter()
            .any(|(_, r)| r.status == RefStatus::Modified)
        {
            "modified"
        } else {
            continue;
        };

        let Some(reason) = classify_note_pure(&note.kind, &note.scope, ref_status, &resolved)
        else {
            continue;
        };

        let mut valid_refs = Vec::new();
        let mut broken_refs = Vec::new();
        for (tok, rr) in &resolved {
            if rr.status == RefStatus::Broken {
                broken_refs.push(BrokenRefEntry {
                    token: tok.clone(),
                    failure_kind: rr.failure_kind.map_or("missing_code_path", |k| k.as_str()),
                });
            } else {
                valid_refs.push(tok.clone());
            }
        }

        let recall_count = recalls_map.get(&note.key).copied().unwrap_or(0);
        all_items.push(TriageItem {
            key: note.key,
            rev: note.rev,
            kind: note.kind,
            scope: note.scope,
            age_days: ((now - note.created_at).max(0)) / 86_400,
            recall_count,
            ref_status,
            triage_reason: reason,
            triage_label: reason.label(),
            suggested_action: reason.action(),
            valid_refs,
            broken_refs,
            commit_context: None,
            symbol_candidates: Vec::new(),
        });
    }

    let total_candidates = all_items.len();
    all_items.truncate(limit);
    let omitted_by_limit = total_candidates.saturating_sub(all_items.len());
    let next_after = if omitted_by_limit > 0 {
        all_items.last().map(|i| i.key.clone())
    } else {
        None
    };

    if let Some(iconn) = index_conn.as_deref() {
        let mut lookups = 0usize;
        for item in &mut all_items {
            for br in &item.broken_refs {
                if lookups >= TRIAGE_MAX_SYMBOL_LOOKUPS {
                    break;
                }
                if matches!(br.failure_kind, "missing_symbol" | "missing_code_path") {
                    lookups += 1;
                    for cand in lookup_symbol_candidates(iconn, &br.token) {
                        if !item.symbol_candidates.contains(&cand)
                            && item.symbol_candidates.len() < 5
                        {
                            item.symbol_candidates.push(cand);
                        }
                    }
                }
            }
        }
    }

    let git_stats = if *global {
        GitStats {
            disabled: true,
            ..Default::default()
        }
    } else {
        crate::memory::triage::git::enrich_with_git(
            root.as_deref(),
            &mut all_items,
            *no_git,
            crate::memory::triage::git::GitBudget::default(),
        )
    };

    let shown = all_items.len();
    let packet = TriagePacket {
        schema_version: 1,
        untrusted_data: true,
        store: if *global { "global" } else { "local" },
        index_state,
        git_stats,
        total_candidates,
        shown,
        omitted_count: omitted_by_limit,
        next_after_key: next_after,
        truncated: omitted_by_limit > 0,
        items: all_items,
    };

    crate::journal::record_result(
        crate::journal::OutcomeCounts::of(shown, total_candidates),
        false,
        0,
    );
    let emit_json = *json || surface == Surface::Mcp;
    let stdout = if emit_json {
        render_triage_json(packet)
    } else {
        render_triage_text(&packet)
    };
    Ok(ArmOut::plain(stdout, String::new()))
}
