//! Memory queries: ranked FTS search, `--all` listings, multi-store merge,
//! stale-ref checks, and row rendering.

use super::scope::StoreScope;
use super::*;

/// Ranked search or the explicit `--all` listing.
pub(super) fn run_mem_query(req: &CommandRequest, root_arg: Option<&Path>) -> Result<ArmOut> {
    let CommandRequest::MemQuery {
        terms,
        all,
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
        unreachable!("run_mem_query called with a non-query request")
    };
    let store_scope = StoreScope::from_flags(*local, *global)?;
    let window = crate::memory::TimeWindow {
        since: *since,
        until: *until,
    };
    crate::memory::validate_window(&window, "mem query")?;
    // Whitespace-only terms normalize to the --all listing instead of exit 2:
    // agents pass blank query strings when they mean "show me the notes".
    let clean_terms: Vec<String> = terms
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let effective_all = *all || clean_terms.is_empty();
    validate_mem_query(&clean_terms, *all, effective_all, *include_superseded)?;
    if *explain && effective_all {
        return Err(EgError::Usage(
            "mem query: --explain requires terms".to_string(),
        ));
    }
    let (root, index_conn) = stale_ref_root_and_conn(root_arg);
    let args = MemQueryArgs {
        terms: &clean_terms,
        all: effective_all,
        limit: (*limit).clamp(1, crate::memory::MAX_MEM_LIMIT),
        filter: crate::db::MemFilter {
            kind: *kind,
            scope: *scope,
            now: crate::db::unix_now(),
            include_superseded: *include_superseded,
            window,
        },
        index_conn: index_conn.as_ref(),
        root: root.as_deref(),
        reference: reference.as_deref(),
        explain: *explain,
        legacy: *legacy,
    };

    match store_scope {
        StoreScope::Local => {
            let root = crate::walk::discover_root(root_arg)?;
            let conn = crate::db::open_memory_query(&root)?;
            query_single_store(&conn, &args, false)
        }
        StoreScope::Global => {
            let conn = crate::db::open_global_memory_query()?;
            query_single_store(&conn, &args, true)
        }
        StoreScope::Merged => {
            let stores = resolve_memory_stores(root_arg)?;
            match (stores.local, stores.global) {
                (None, None) => Err(no_memory_store_error(root_arg)),
                (Some(local), None) => query_single_store(&local, &args, false),
                (None, Some(global)) => query_single_store(&global, &args, true),
                (Some(local), Some(global)) => query_merged_stores(&local, &global, &args),
            }
        }
    }
}

// The ref-status helper lives in the db layer (`crate::db::query_state`)
// so the reuse allowlist can consume it without a query->service edge.
#[cfg(test)]
use crate::db::check_ref_status;
use crate::db::{check_ref_status_with_root, RefStatus};

pub(super) fn check_note_ref_status(
    index_conn: Option<&rusqlite::Connection>,
    root: Option<&Path>,
    row: &crate::db::MemRow,
) -> RefStatus {
    let Some(conn) = index_conn else {
        return RefStatus::Valid;
    };
    if row.refs.trim().is_empty() {
        return RefStatus::Valid;
    }
    let mut has_modified = false;
    for r in row.refs.split_whitespace() {
        match check_ref_status_with_root(conn, root, r, row.anchor()) {
            RefStatus::Broken => return RefStatus::Broken,
            RefStatus::Modified => has_modified = true,
            RefStatus::Valid => {}
        }
    }
    if has_modified {
        RefStatus::Modified
    } else {
        RefStatus::Valid
    }
}

/// The `[global] [stale-ref]` store markers shared by `mem get` output and
/// the listing tag builder, so the suffix order cannot drift.
pub(super) fn store_tags(is_global: bool, status: RefStatus, source: &str) -> String {
    let mut tag = String::new();
    if source == "import" {
        tag.push_str(" [imported]");
    }
    if is_global {
        tag.push_str(" [global]");
    }
    match status {
        RefStatus::Broken => tag.push_str(" [stale-ref] [broken-ref]"),
        RefStatus::Modified => tag.push_str(" [stale-ref] [modified-ref]"),
        RefStatus::Valid => {}
    }
    tag
}

/// The `[superseded]`/`[global]`/`[stale-ref]` listing tag for one row, shared
/// by the single-store and merged listings so the markers cannot drift.
fn mem_row_tag(row: &crate::db::MemRow, is_global: bool, status: RefStatus) -> String {
    let mut tag = String::new();
    if row.superseded_by.is_some() {
        tag.push_str(" [superseded]");
    }
    tag.push_str(&store_tags(is_global, status, &row.source));
    tag
}

/// The caveated empty error for the `--all` listing, keyed on whether the
/// listing extended to superseded revisions.
fn mem_all_empty(include_superseded: bool) -> EgError {
    mem_empty(if include_superseded {
        "memory store has no notes"
    } else {
        "memory store has no current notes"
    })
}

/// The listing tail shared by the single-store and merged queries: the
/// empty guard with the caller's caveated error, the cap, the per-store
/// tag, and the journal. Each row carries its store marker (`true` = global).
fn render_mem_rows(
    rows: Vec<(crate::db::MemRow, bool)>,
    args: &MemQueryArgs<'_>,
    explain: Option<&std::collections::HashMap<(bool, i64), String>>,
    empty_error: impl FnOnce() -> EgError,
) -> Result<ArmOut> {
    let rows: Vec<_> = if let Some(target_ref) = args.reference {
        rows.into_iter()
            .filter(|(row, _)| crate::memory::ref_matches_target(&row.refs, target_ref))
            .collect()
    } else {
        rows
    };
    if rows.is_empty() {
        return Err(empty_error());
    }
    let total = rows.len();
    let shown = total.min(args.limit);
    let rendered: Vec<String> = rows
        .iter()
        .take(shown)
        .map(|(row, is_global)| {
            let status = check_note_ref_status(args.index_conn, args.root, row);
            let mut line =
                crate::memory::render_row_tagged(row, &mem_row_tag(row, *is_global, status));
            if let Some(map) = explain {
                if let Some(why) = map.get(&(*is_global, row.id)) {
                    line.push_str("\n  why: ");
                    line.push_str(why);
                }
            }
            line
        })
        .collect();
    Ok(render_mem_list(&rendered, total, shown))
}

/// The ranked tail shared by the single-store and merged listings: fuse,
/// cap, render with the per-candidate store tag, and journal.
fn render_ranked(
    candidates: Vec<crate::memory::Candidate>,
    args: &MemQueryArgs<'_>,
) -> Result<ArmOut> {
    let explain = args.explain.then(|| {
        super::explain::build(
            &candidates,
            &args.terms.join(" "),
            args.filter.now,
            args.index_conn.is_some(),
            args.legacy,
        )
    });
    let fused = crate::memory::fuse(candidates);
    render_mem_rows(
        fused
            .into_iter()
            .map(|cand| (cand.row, cand.is_global))
            .collect(),
        args,
        explain.as_ref(),
        || mem_empty(&format!("no memory matches '{}'", args.terms.join(" "))),
    )
}

/// The `--all` listing tail shared by the single-store and merged queries:
/// the caveated empty error keyed on superseded revisions.
fn render_all_rows(
    rows: Vec<(crate::db::MemRow, bool)>,
    args: &MemQueryArgs<'_>,
) -> Result<ArmOut> {
    render_mem_rows(rows, args, None, || {
        mem_all_empty(args.filter.include_superseded)
    })
}

fn query_single_store(
    conn: &rusqlite::Connection,
    args: &MemQueryArgs<'_>,
    is_global: bool,
) -> Result<ArmOut> {
    if args.all {
        let rows = crate::db::memory_current_rows(conn, &args.filter, None)?;
        return render_all_rows(rows.into_iter().map(|row| (row, is_global)).collect(), args);
    }
    let joined = args.terms.join(" ");
    let query_vec = crate::memory::embed::embed_text(&joined);
    let candidates = mem_ranked(
        conn,
        &joined,
        &args.filter,
        is_global,
        args.index_conn,
        args.legacy,
        query_vec.as_deref(),
    )?;
    render_ranked(candidates, args)
}

/// Local-first `--all` merge of the two stores: local current rows, then
/// global rows whose key the local store does not carry, newest first with
/// the key as tie-breaker. Each row keeps its store marker.
fn merge_all_rows(
    local_conn: &rusqlite::Connection,
    global_conn: &rusqlite::Connection,
    filter: &crate::db::MemFilter,
) -> Result<Vec<(crate::db::MemRow, bool)>> {
    let local_rows = crate::db::memory_current_rows(local_conn, filter, None)?;
    let global_rows = crate::db::memory_current_rows(global_conn, filter, None)?;
    let mut seen_keys = std::collections::HashSet::new();
    let mut combined = Vec::new();
    for row in local_rows {
        seen_keys.insert(row.key.clone());
        combined.push((row, false));
    }
    for row in global_rows {
        if !seen_keys.contains(&row.key) {
            combined.push((row, true));
        }
    }
    combined.sort_by(|(a, _), (b, _)| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.key.cmp(&b.key))
    });
    Ok(combined)
}

fn query_merged_stores(
    local_conn: &rusqlite::Connection,
    global_conn: &rusqlite::Connection,
    args: &MemQueryArgs<'_>,
) -> Result<ArmOut> {
    if args.all {
        let combined = merge_all_rows(local_conn, global_conn, &args.filter)?;
        return render_all_rows(combined, args);
    }
    let joined = args.terms.join(" ");
    let query_vec = crate::memory::embed::embed_text(&joined);
    let mut combined = mem_ranked(
        local_conn,
        &joined,
        &args.filter,
        false,
        args.index_conn,
        args.legacy,
        query_vec.as_deref(),
    )?;
    let seen_keys: std::collections::HashSet<String> =
        combined.iter().map(|c| c.row.key.clone()).collect();
    let global = mem_ranked(
        global_conn,
        &joined,
        &args.filter,
        true,
        args.index_conn,
        args.legacy,
        query_vec.as_deref(),
    )?;
    // One batched existence probe replaces the per-candidate `memory_get`
    // (N+1 with a revision COUNT per key): only keys the local ranking has
    // not already surfaced need the shadowing check.
    let probe: Vec<String> = global
        .iter()
        .map(|c| c.row.key.clone())
        .filter(|key| !seen_keys.contains(key))
        .collect();
    let local_keys: std::collections::HashSet<String> =
        crate::db::memory_current_keys(local_conn, &probe, &args.filter.window, args.filter.now)?
            .into_iter()
            .collect();
    for c in global {
        if seen_keys.contains(&c.row.key) || local_keys.contains(&c.row.key) {
            continue;
        }
        combined.push(c);
    }
    render_ranked(combined, args)
}

/// Reject the flag combinations the two query modes cannot serve.
fn validate_mem_query(
    terms: &[String],
    explicit_all: bool,
    effective_all: bool,
    include_superseded: bool,
) -> Result<()> {
    if explicit_all && !terms.is_empty() {
        return Err(EgError::Usage(
            "mem query: --all and terms are mutually exclusive".to_string(),
        ));
    }
    // Superseded rows participate in the explicit listing only; the
    // ranked resolver keeps supersession-at-result-time exclusion.
    if include_superseded && !effective_all {
        return Err(EgError::Usage(
            "mem query: --include-superseded requires --all".to_string(),
        ));
    }
    Ok(())
}

/// Shared cap, journal, and stderr contract for the two mem query modes.
/// `rendered` holds exactly `shown` rows; `total` drives the cap footer.
fn render_mem_list(rendered: &[String], total: usize, shown: usize) -> ArmOut {
    let mut stdout = String::new();
    stdout.push_str(crate::memory::MEMORY_CAVEAT);
    stdout.push('\n');
    for line in rendered {
        stdout.push_str(line);
        stdout.push('\n');
    }
    if total > shown {
        stdout.push_str(&crate::query::cap_line(shown, total, "").unwrap_or_default());
        stdout.push('\n');
    }
    crate::journal::record_result(crate::journal::OutcomeCounts::of(shown, total), false, 0);
    ArmOut::plain(stdout, crate::query::cap_stderr(shown, total))
}

/// One mem get hit: journal, the caveat, the tagged row, and the revision
/// count (`tag` is the store marker, "" for local and " [global]" for global).
pub(super) fn render_mem_hit(row: &crate::db::MemRow, tag: &str, revs: i64) -> ArmOut {
    crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
    ArmOut::plain(
        format!(
            "{}\n{}\nrevs: {}\n",
            crate::memory::MEMORY_CAVEAT,
            crate::memory::render_row_tagged(row, tag),
            revs
        ),
        String::new(),
    )
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod embed_tests {
    use super::*;
    use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn query_merged_stores_embeds_query_once() {
        let root = temp_root("merged-single-embed");
        let model_dir = root.join("model");
        make_dir_all(&model_dir).unwrap();
        write_file(model_dir.join("lazy.json"), b"{}").unwrap();

        let count_file = root.join("embed_calls.log");
        let bin = root.join("eg-judge");
        let script = format!(
            "#!/bin/sh\necho call >> '{}'\nprintf '{{\"dim\":2,\"embedding\":[1.0,0.0]}}\\n'\n",
            count_file.display()
        );
        write_file(&bin, script.as_bytes()).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prev_model = std::env::var("EG_JUDGE_TEXT_MODEL").ok();
        let prev_bin = std::env::var("EG_JUDGE_BIN").ok();
        std::env::set_var("EG_JUDGE_TEXT_MODEL", &model_dir);
        std::env::set_var("EG_JUDGE_BIN", &bin);

        let local_root = root.join("local");
        let global_root = root.join("global");
        make_dir_all(&local_root).unwrap();
        make_dir_all(&global_root).unwrap();
        let mut local_conn = crate::db::open_memory_store(&local_root).unwrap();
        let mut global_conn = crate::db::open_memory_store(&global_root).unwrap();

        let now = 1_000_000;
        let note = |key: &'static str, body: &'static str| crate::db::MemNoteInput {
            key,
            kind: "fact",
            tags: "",
            refs: "",
            scope: "durable",
            session: "",
            body,
            source: "cli",
            expires_at: 0,
            created_at: None,
            embedding: None,
            supersedes: None,
            touch: false,
        };
        crate::db::memory_write(&mut local_conn, &note("l1", "alpha local note"), now).unwrap();
        crate::db::memory_write(&mut global_conn, &note("g1", "alpha global note"), now).unwrap();

        let terms = vec!["alpha".to_string()];
        let args = MemQueryArgs {
            terms: &terms,
            all: false,
            limit: 10,
            filter: crate::db::MemFilter {
                kind: None,
                scope: None,
                now: now + 1,
                include_superseded: false,
                window: crate::memory::TimeWindow::default(),
            },
            index_conn: None,
            root: None,
            reference: None,
            explain: false,
            legacy: false,
        };
        let _ = query_merged_stores(&local_conn, &global_conn, &args).unwrap();

        match prev_model {
            Some(v) => std::env::set_var("EG_JUDGE_TEXT_MODEL", v),
            None => std::env::remove_var("EG_JUDGE_TEXT_MODEL"),
        }
        match prev_bin {
            Some(v) => std::env::set_var("EG_JUDGE_BIN", v),
            None => std::env::remove_var("EG_JUDGE_BIN"),
        }

        let calls = std::fs::read_to_string(&count_file).unwrap_or_default();
        assert_eq!(
            calls.lines().count(),
            1,
            "expected 1 embed call, got: {calls}"
        );
        remove_tree(&root);
    }
}
