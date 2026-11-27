//! Counts-only memory snapshot for audit. Reads current, unexpired refs and
//! resolves them against read-only indexes; never materializes note bodies,
//! keys, tags, or paths.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::db;

use super::collect::Mode;

#[derive(Default)]
pub struct MemoryFacts {
    pub failure: Option<MemoryFailure>,
    pub stores_checked: usize,
    pub stores_truncated: bool,
    pub broken: usize,
    pub modified: usize,
    pub valid: usize,
    pub unverified: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MemoryFailure {
    Corrupt,
    Schema,
    Symlink,
    Tracked,
    TooLarge,
    Unreadable,
}

struct Store {
    repo: Option<PathBuf>,
    path: PathBuf,
}

pub fn collect(mode: &Mode, repos: &[PathBuf], now: i64) -> MemoryFacts {
    let mut stores: Vec<Store> = Vec::new();
    match mode {
        Mode::Local(root) => stores.push(Store {
            repo: Some(root.clone()),
            path: db::memory_db_path(root),
        }),
        Mode::Global => {
            if let Ok(g) = db::global_eg_dir() {
                stores.push(Store {
                    repo: None,
                    path: g.join("memory.db"),
                });
            }
            for repo in repos {
                stores.push(Store {
                    repo: Some(repo.clone()),
                    path: db::memory_db_path(repo),
                });
            }
        }
    }
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut unique: Vec<Store> = Vec::new();
    for store in stores {
        // Keep symlinks so the integrity check can report them; canonicalize
        // only regular files for dedup.
        if symlink_store(&store.path) {
            unique.push(store);
            continue;
        }
        if let Ok(p) = std::fs::canonicalize(&store.path) {
            if seen.insert(p) {
                unique.push(store);
            }
        }
    }
    let truncated = unique.len() > 50;
    unique.truncate(50);
    let mut facts = MemoryFacts {
        stores_truncated: truncated,
        ..Default::default()
    };
    for store in &unique {
        if !regular_store(&store.path) {
            if symlink_store(&store.path) {
                facts.failure = Some(MemoryFailure::Symlink);
            }
            continue;
        }
        if let Some(repo) = &store.repo {
            if db::reject_tracked_eg_dir(repo).is_err() {
                facts.failure = Some(MemoryFailure::Tracked);
                continue;
            }
        }
        let size = db::file_size(&store.path);
        if size > 50 * 1024 * 1024 {
            facts.failure = Some(MemoryFailure::TooLarge);
            continue;
        }
        facts.stores_checked += 1;
        if let Err(kind) = inspect_store(store, now, &mut facts) {
            facts.failure = Some(kind);
        }
    }
    facts
}

fn inspect_store(store: &Store, now: i64, facts: &mut MemoryFacts) -> Result<(), MemoryFailure> {
    let conn = open_store(store)?;
    let integrity: std::result::Result<String, _> =
        conn.query_row("PRAGMA integrity_check", [], |r| r.get(0));
    if !matches!(integrity, Ok(ref v) if v == "ok") {
        return Err(MemoryFailure::Corrupt);
    }
    let index = store.repo.as_ref().and_then(|r| db::try_open_index(r).ok());
    // v1 stores predate anchored_at; read the creation time as the anchor
    // instead of failing the whole store on a missing column.
    let anchor_expr =
        if crate::db::column_present(&conn, "memories", "anchored_at").unwrap_or(false) {
            "anchored_at"
        } else {
            "0"
        };
    let sql = format!(
        "SELECT refs, {anchor_expr}, created_at FROM memories \
         WHERE superseded_by IS NULL AND (expires_at = 0 OR expires_at > ?1) \
         ORDER BY key ASC, id ASC LIMIT 500"
    );
    let mut stmt = conn.prepare(&sql).map_err(|_| MemoryFailure::Corrupt)?;
    let rows = stmt
        .query_map(rusqlite::params![now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| MemoryFailure::Corrupt)?;
    let mut ref_cache: std::collections::HashMap<String, db::ResolvedRef> =
        std::collections::HashMap::new();
    for row in rows {
        let (refs, anchored_at, created_at) = row.map_err(|_| MemoryFailure::Corrupt)?;
        let anchor = anchored_at.max(created_at);
        for r in refs.split_whitespace().take(20) {
            let resolved = ref_cache
                .entry(r.to_string())
                .or_insert_with(|| {
                    db::resolve_ref_detail(index.as_ref(), store.repo.as_deref(), r, anchor)
                })
                .with_anchor(anchor);
            match resolved.status {
                db::RefStatus::Broken => facts.broken += 1,
                db::RefStatus::Modified => facts.modified += 1,
                db::RefStatus::Valid => facts.valid += 1,
                db::RefStatus::Unverified => facts.unverified += 1,
            }
        }
    }
    Ok(())
}

fn open_store(store: &Store) -> Result<Connection, MemoryFailure> {
    match store.repo.as_ref() {
        None => db::open_global_memory_query().map_err(map_open_error),
        Some(repo) => db::open_memory_query(repo).map_err(map_open_error),
    }
}

fn map_open_error(e: crate::error::EgError) -> MemoryFailure {
    match e {
        crate::error::EgError::SchemaMismatch { .. } => MemoryFailure::Schema,
        crate::error::EgError::Security(_) => MemoryFailure::Symlink,
        _ => MemoryFailure::Corrupt,
    }
}

fn regular_store(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

fn symlink_store(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};

    #[test]
    fn audit_resolves_non_indexed_disk_ref_as_valid_or_modified() {
        let root = temp_root("audit-disk-ref");
        make_dir_all(root.join("docs")).unwrap();
        write_file(root.join("docs/guide.md"), b"# Guide\n").unwrap();
        let mtime = crate::db::check_disk_ref(&root, "docs/guide.md").unwrap().1;

        let mut mem = crate::db::open_memory_store(&root).unwrap();
        crate::db::memory_write(
            &mut mem,
            &crate::db::MemNoteInput {
                key: "doc-note",
                kind: "fact",
                tags: "",
                refs: "docs/guide.md",
                scope: "durable",
                session: "",
                body: "doc ref note",
                source: "cli",
                expires_at: 0,
                created_at: None,
                embedding: None,
                supersedes: None,
                touch: false,
            },
            mtime + 10,
        )
        .unwrap();
        drop(mem);

        let facts = collect(&Mode::Local(root.clone()), &[], mtime + 20);
        assert_eq!(facts.valid, 1);
        assert_eq!(facts.broken, 0);

        remove_tree(&root);
    }

    #[test]
    fn check_ref_status_with_root_resolves_non_indexed_disk_files() {
        let root = temp_root("disk-ref-status");
        make_dir_all(root.join("docs")).unwrap();
        let md = root.join("docs/guide.md");
        write_file(&md, b"# Guide\n").unwrap();
        let html = root.join("report.html");
        write_file(&html, b"<html></html>").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../schema/index.sql"))
            .unwrap();
        let mtime = crate::db::check_disk_ref(&root, "docs/guide.md").unwrap().1;

        assert_eq!(
            crate::db::check_ref_status_with_root(&conn, Some(&root), "docs/guide.md", mtime),
            crate::db::RefStatus::Valid
        );
        assert_eq!(
            crate::db::check_ref_status_with_root(&conn, Some(&root), "report.html", mtime - 10),
            crate::db::RefStatus::Modified
        );
        assert_eq!(
            crate::db::check_ref_status_with_root(&conn, Some(&root), "docs/missing.md", mtime),
            crate::db::RefStatus::Broken
        );
        assert_eq!(
            crate::db::check_ref_status_with_root(&conn, Some(&root), "../outside.md", mtime),
            crate::db::RefStatus::Broken
        );
        assert_eq!(
            crate::db::check_ref_status_with_root(&conn, Some(&root), "/etc/passwd", mtime),
            crate::db::RefStatus::Broken
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&md, root.join("docs/link.md")).unwrap();
            assert_eq!(
                crate::db::check_ref_status_with_root(&conn, Some(&root), "docs/link.md", mtime),
                crate::db::RefStatus::Broken
            );
        }

        remove_tree(&root);
    }
}
