//! Memory-store aggregation for the dashboard memories panel.
//! Correlates local and global memory notes with recall telemetry from
//! usage entries and verifies ref targets against the code index.
//!
//! Payload contract (`/api/memories`): `{schema_version, notes, degraded,
//! errors}`. A store that cannot be read contributes an `errors` entry and a
//! `degraded` flag instead of silently rendering as an empty store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db;
use crate::error::Result;

/// One discovered memory store. `store_id` is the canonical `memory.db` path:
/// a stable address, unlike the display name a request must never be keyed by.
struct Store {
    name: String,
    repo_root: PathBuf,
    db_path: PathBuf,
    store_id: String,
    /// Repo root whose `.engram/` owns this store; `None` for the global store.
    guard_root: Option<PathBuf>,
}

pub fn collect_memories(root: Option<&Path>, entries: &[Value]) -> Result<Value> {
    let stores = discover_memory_stores(root, entries);
    let recalls_map = aggregate_recalls(entries);
    let mut notes_json = Vec::new();
    let mut errors = Vec::new();

    for store in &stores {
        if let Err(err) = collect_store_notes(store, &recalls_map, &mut notes_json) {
            errors.push(err);
        }
    }

    let degraded = !errors.is_empty();
    Ok(json!({
        "schema_version": 2,
        "notes": notes_json,
        "degraded": degraded,
        "errors": errors,
    }))
}

/// Resolve one `store_id` from the same discovery the panel uses. Touch uses
/// this so a name can never pick a different store than the one on screen.
pub fn resolve_store(
    root: Option<&Path>,
    entries: &[Value],
    store_id: &str,
) -> Option<(String, PathBuf, PathBuf)> {
    discover_memory_stores(root, entries)
        .into_iter()
        .find(|s| s.store_id == store_id)
        .map(|s| (s.name, s.repo_root, s.db_path))
}

/// Every addressable store id, for a 404 that tells the operator what exists.
pub fn store_ids(root: Option<&Path>, entries: &[Value]) -> Vec<String> {
    discover_memory_stores(root, entries)
        .into_iter()
        .map(|s| s.store_id)
        .collect()
}

/// The global root IS the `.engram` directory: its store sits directly under
/// it. Re-joining `.engram` here once produced `~/.engram/.engram/memory.db`.
fn global_store_db(g_dir: &Path) -> PathBuf {
    g_dir.join("memory.db")
}

fn store_id_for(db_path: &Path) -> String {
    std::fs::canonicalize(db_path)
        .unwrap_or_else(|_| db_path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn discover_memory_stores(root: Option<&Path>, entries: &[Value]) -> Vec<Store> {
    let mut stores: Vec<Store> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();

    // `db_path` is explicit: the global root already IS the `.engram`
    // directory, so re-joining `.engram` here doubled the home path.
    let mut push =
        |name: String, repo_root: PathBuf, db_path: PathBuf, guard_root: Option<PathBuf>| {
            let store_id = store_id_for(&db_path);
            if seen.insert(store_id.clone(), ()).is_none() {
                stores.push(Store {
                    name,
                    repo_root,
                    db_path,
                    store_id,
                    guard_root,
                });
            }
        };

    if let Ok(g_dir) = db::global_eg_dir() {
        let g_db = global_store_db(&g_dir);
        if g_db.is_file() {
            push("global".to_string(), g_dir, g_db, None);
        }
    }

    let mut locals: HashMap<String, PathBuf> = HashMap::new();
    super::discover_repos(root, entries, "memory.db", &mut locals);
    // Keys are repository paths; labels are assigned over the sorted set so
    // same-basename checkouts stay distinct stores with distinct labels.
    let mut paths: Vec<String> = locals.keys().cloned().collect();
    paths.sort();
    let mut taken = std::collections::HashSet::new();
    for path in paths {
        if let Some(repo) = locals.remove(&path) {
            let name = super::display_name_for(Path::new(&path), &mut taken);
            let db_path = repo.join(".engram").join("memory.db");
            push(name, repo.clone(), db_path, Some(repo));
        }
    }

    stores
}

/// Exact-key recall counts only. A bare store-wide query cannot be attributed
/// to one note, and a substring guess is not a measurement. Only successful
/// lookups count: a `mem_get` that exited 1 (key unknown) or 3 (store error)
/// delivered nothing, so it is not a recall.
fn aggregate_recalls(entries: &[Value]) -> HashMap<String, (usize, f64)> {
    let mut recalls_map: HashMap<String, (usize, f64)> = HashMap::new();
    for e in entries {
        let tool = e.get("tool").and_then(Value::as_str).unwrap_or("");
        if tool != "mem_get" && tool != "mem_query" {
            continue;
        }
        if e.get("exit_code").and_then(Value::as_i64).unwrap_or(0) != 0 {
            continue;
        }
        if e.get("result_count").and_then(Value::as_u64).unwrap_or(0) == 0 {
            continue;
        }
        let ts = e.get("ts").and_then(Value::as_f64).unwrap_or(0.0);
        let key = e
            .get("args")
            .and_then(|a| a.get("key"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if key.is_empty() {
            continue;
        }
        let rec = recalls_map.entry(key.to_string()).or_insert((0, 0.0));
        rec.0 += 1;
        if ts > rec.1 {
            rec.1 = ts;
        }
    }
    recalls_map
}

pub(crate) fn resolve_single_ref(
    r: &str,
    index_conn: Option<&Connection>,
    root: Option<&Path>,
    anchor: i64,
) -> Value {
    let Some(iconn) = index_conn else {
        if let Some(repo_root) = root {
            if let Some((size, mtime_secs)) = crate::db::check_disk_ref(repo_root, r) {
                let is_modified = mtime_secs > anchor;
                return json!({
                    "ref": r,
                    "verified": true,
                    "status": if is_modified { "modified" } else { "valid" },
                    "type": "file",
                    "location": format!("{r} ({size} bytes)")
                });
            }
        }
        // No index for this store (global, or a repo that was never built):
        // verification is impossible, not failed.
        return json!({
            "ref": r, "verified": false, "status": "unverified",
            "type": "path", "location": ""
        });
    };

    if let Ok(mut f_stmt) = iconn.prepare(
        "SELECT path, size, mtime FROM files
         WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'
         ORDER BY (path = ?1) DESC, mtime DESC, path ASC LIMIT 1",
    ) {
        if let Ok((path, size, mtime_nanos)) = f_stmt.query_row(
            rusqlite::params![r, format!("%/{}", crate::db::like_escape(r))],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        ) {
            let is_modified = (mtime_nanos / 1_000_000_000) > anchor;
            return json!({
                "ref": r,
                "verified": true,
                "status": if is_modified { "modified" } else { "valid" },
                "type": "file",
                "location": format!("{path} ({size} bytes)")
            });
        }
    }

    if let Ok(mut s_stmt) = iconn.prepare(
        "SELECT f.path, s.line_start, s.line_end, f.mtime FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.name = ?1 OR s.qualname = ?1
         ORDER BY s.line_start ASC, f.path ASC, s.qualname ASC LIMIT 1",
    ) {
        if let Ok((fp, ls, le, mtime_nanos)) = s_stmt.query_row(rusqlite::params![r], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
        }) {
            let is_modified = (mtime_nanos / 1_000_000_000) > anchor;
            return json!({
                "ref": r,
                "verified": true,
                "status": if is_modified { "modified" } else { "valid" },
                "type": "symbol",
                "location": format!("{fp}:{ls}-{le}")
            });
        }
    }

    if let Some(repo_root) = root {
        if let Some((size, mtime_secs)) = crate::db::check_disk_ref(repo_root, r) {
            let is_modified = mtime_secs > anchor;
            return json!({
                "ref": r,
                "verified": true,
                "status": if is_modified { "modified" } else { "valid" },
                "type": "file",
                "location": format!("{r} ({size} bytes)")
            });
        }
    }

    json!({
        "ref": r, "verified": false, "status": "broken", "type": "path", "location": ""
    })
}

fn resolve_note_refs(
    refs_list: &[String],
    index_conn: Option<&Connection>,
    root: Option<&Path>,
    anchor: i64,
) -> Vec<Value> {
    refs_list
        .iter()
        .map(|r| resolve_single_ref(r, index_conn, root, anchor))
        .collect()
}

struct MemoryRow {
    id: i64,
    key: String,
    rev: i64,
    kind: String,
    scope: String,
    body: String,
    tags_raw: String,
    refs_raw: String,
    source: String,
    created_at: i64,
    anchored_at: i64,
    is_current: bool,
}

fn format_note_json(
    store: &Store,
    row: MemoryRow,
    index_conn: Option<&Connection>,
    recalls_map: &HashMap<String, (usize, f64)>,
) -> Value {
    let tags: Vec<String> = row
        .tags_raw
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let refs_list: Vec<String> = row
        .refs_raw
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let (recall_count, last_recalled_ts) = recalls_map.get(&row.key).copied().unwrap_or((0, 0.0));

    // Staleness is measured against the last verify, falling back to the
    // creation date for notes that were never anchored.
    let anchor = if row.anchored_at > 0 {
        row.anchored_at
    } else {
        row.created_at
    };
    let resolved_refs =
        resolve_note_refs(&refs_list, index_conn, store.guard_root.as_deref(), anchor);
    let ref_status = if resolved_refs
        .iter()
        .any(|r| r.get("status").and_then(Value::as_str) == Some("broken"))
    {
        "broken"
    } else if resolved_refs
        .iter()
        .any(|r| r.get("status").and_then(Value::as_str) == Some("modified"))
    {
        "modified"
    } else if resolved_refs
        .iter()
        .any(|r| r.get("status").and_then(Value::as_str) == Some("unverified"))
    {
        "unverified"
    } else if !resolved_refs.is_empty() {
        "valid"
    } else {
        "none"
    };

    json!({
        "id": row.id,
        "store": store.name,
        "store_id": store.store_id,
        "key": row.key,
        "rev": row.rev,
        "kind": row.kind,
        "scope": row.scope,
        "body": row.body,
        "tags": tags,
        "refs": refs_list,
        "resolved_refs": resolved_refs,
        "ref_status": ref_status,
        "source": row.source,
        "created_at": row.created_at,
        "anchored_at": row.anchored_at,
        "is_current": row.is_current,
        "recall_count": recall_count,
        "last_recalled_ts": last_recalled_ts,
    })
}

fn collect_store_notes(
    store: &Store,
    recalls_map: &HashMap<String, (usize, f64)>,
    out: &mut Vec<Value>,
) -> std::result::Result<(), Value> {
    let err = |stage: &str, message: String| json!({ "store": store.name, "store_id": store.store_id, "stage": stage, "message": message });
    if let Some(root) = &store.guard_root {
        crate::db::reject_symlink(&root.join(".engram"), ".engram directory")
            .map_err(|e| err("security", e.to_string()))?;
        crate::db::reject_tracked_eg_dir(root).map_err(|e| err("security", e.to_string()))?;
    }
    crate::db::reject_symlink(&store.db_path, "memory store")
        .map_err(|e| err("security", e.to_string()))?;
    let conn =
        Connection::open_with_flags(&store.db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| err("open", e.to_string()))?;
    let index_path = store.repo_root.join(".engram").join("index.db");
    let index_conn = if crate::db::reject_symlink(&index_path, "index.db").is_ok() {
        Connection::open_with_flags(&index_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
    } else {
        None
    };

    // v1 stores predate anchored_at; fall back to created_at at read time
    // instead of migrating a read-only connection.
    let anchor_expr =
        if crate::db::column_present(&conn, "memories", "anchored_at").unwrap_or(false) {
            "anchored_at"
        } else {
            "0"
        };
    // An expired session note is not current even though nothing superseded
    // it. v1 stores predate the TTL column: nothing there can be expired, so
    // `is_current` stays the supersession predicate alone.
    let has_expiry = crate::db::column_present(&conn, "memories", "expires_at").unwrap_or(false);
    let current_expr = if has_expiry {
        "(superseded_by IS NULL AND (expires_at = 0 OR expires_at > ?1))"
    } else {
        "(superseded_by IS NULL AND 1)"
    };
    let sql = format!(
        "SELECT id, key, rev, kind, scope, body, tags, refs, source, created_at, {anchor_expr}, \
         {current_expr} FROM memories ORDER BY key ASC, rev DESC"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| err("prepare", e.to_string()))?;

    let decode = |r: &rusqlite::Row<'_>| -> rusqlite::Result<MemoryRow> {
        Ok(MemoryRow {
            id: r.get(0)?,
            key: r.get(1)?,
            rev: r.get(2)?,
            kind: r.get(3)?,
            scope: r.get(4)?,
            body: r.get(5)?,
            tags_raw: r.get(6)?,
            refs_raw: r.get(7)?,
            source: r.get(8)?,
            created_at: r.get(9)?,
            anchored_at: r.get(10)?,
            is_current: r.get::<_, i64>(11)? != 0,
        })
    };
    // Per-branch binding: an unconditional `?1` bind fails InvalidParameterCount
    // exactly on the v1 stores the column guard protects.
    let rows = if has_expiry {
        stmt.query_map([crate::db::unix_now()], decode)
    } else {
        stmt.query_map([], decode)
    };

    let mapped = rows.map_err(|e| err("query", e.to_string()))?;
    for row in mapped {
        match row {
            Ok(row) => out.push(format_note_json(
                store,
                row,
                index_conn.as_ref(),
                recalls_map,
            )),
            Err(e) => return Err(err("decode", e.to_string())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod disk_ref_tests {
    use super::*;
    use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

    #[test]
    fn resolve_single_ref_checks_non_indexed_disk_files() {
        let root = temp_root("dashboard-disk-ref");
        make_dir_all(root.join("docs")).unwrap();
        write_file(root.join("docs/spec.md"), b"# Spec\n").unwrap();
        write_file(root.join("page.html"), b"<html/>").unwrap();

        let iconn = Connection::open_in_memory().unwrap();
        iconn
            .execute_batch(include_str!("../../../schema/index.sql"))
            .unwrap();
        let mtime = crate::db::check_disk_ref(&root, "docs/spec.md").unwrap().1;

        let valid = resolve_single_ref("docs/spec.md", Some(&iconn), Some(&root), mtime);
        assert_eq!(valid["status"], "valid");
        assert_eq!(valid["verified"], true);

        let modified = resolve_single_ref("page.html", Some(&iconn), Some(&root), mtime - 10);
        assert_eq!(modified["status"], "modified");
        assert_eq!(modified["verified"], true);

        let broken = resolve_single_ref("docs/missing.md", Some(&iconn), Some(&root), mtime);
        assert_eq!(broken["status"], "broken");
        assert_eq!(broken["verified"], false);

        remove_tree(&root);
    }
}
