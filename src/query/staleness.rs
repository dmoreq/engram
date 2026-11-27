//! Result-set staleness: the shared changed-since-build header wording and
//! the (mtime, size) stat short-circuit with FNV-1a re-hash on mismatch.

use super::*;

/// Count result files changed since the build and render the standard header.
/// Every query surface uses the same wording and the same stat short-circuit.
pub(super) fn staleness_header(
    ctx: &QueryCtx<'_>,
    files: &BTreeSet<String>,
) -> (Option<String>, bool) {
    if files.is_empty() {
        return (None, false);
    }
    let paths: Vec<&str> = files.iter().map(String::as_str).collect();
    let mut changed = 0usize;
    let _ = crate::db::for_each_chunk(ctx.conn, &paths, |conn, placeholders, chunk| {
        let sql = format!(
            "SELECT path, mtime, size, content_hash FROM files WHERE path IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (path, mtime, size, hash) = row?;
            if file_meta_changed(ctx.root, &path, (mtime, size, hash)) {
                changed += 1;
            }
        }
        Ok(())
    });
    let stale = changed > 0;
    let header = stale.then(|| {
        format!(
            "eg: {changed} of {} result files changed since build — run eg build",
            files.len()
        )
    });
    (header, stale)
}

/// Staleness: (mtime, size) stat short-circuit; FNV-1a re-hash on mismatch.
pub(crate) fn file_changed(ctx: &QueryCtx<'_>, rel: &str) -> bool {
    let row = ctx.conn.query_row(
        "SELECT mtime, size, content_hash FROM files WHERE path = ?1",
        [rel],
        crate::db::ints3,
    );
    let Ok(stored) = row else {
        return false;
    };
    file_meta_changed(ctx.root, rel, stored)
}

/// Compare on-disk metadata/content against stored `(mtime, size, content_hash)`.
pub(crate) fn file_meta_changed(root: &Path, rel: &str, stored: (i64, i64, i64)) -> bool {
    let (stored_mtime, stored_size, stored_hash) = stored;
    // Index rows are attacker-authored in a cloned repository. Never join an
    // absolute, parent-traversing, or backslash path onto the root, and never
    // follow a symlink out of the repository.
    if !safe_index_path(rel) {
        return true;
    }
    let abs = root.join(rel);
    let Ok(meta) = std::fs::symlink_metadata(&abs) else {
        return true;
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return true;
    }
    if meta.len() != stored_size as u64 {
        return true;
    }
    let Some(mtime) = db::metadata_mtime(&meta) else {
        return true;
    };
    if mtime != stored_mtime {
        // Same guard as the walk (regular file + hard 1 MiB cap): a FIFO,
        // socket, or over-cap replacement must not block or blow up the
        // re-hash. Any such oddity is conservatively treated as changed.
        let Ok(bytes) = crate::walk::read_bounded(&abs) else {
            return true;
        };
        let hash = fnv1a64(&bytes) as i64;
        return hash != stored_hash;
    }
    false
}

/// A repo-relative indexed path may contain only normal components. This
/// mirrors the write-path containment rule and rejects Windows separators on
/// every platform so an index built elsewhere cannot escape the root.
pub(crate) fn safe_index_path(rel: &str) -> bool {
    if rel.is_empty() || rel.contains('\\') {
        return false;
    }
    let path = std::path::Path::new(rel);
    if path.is_absolute() {
        return false;
    }
    path.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::safe_index_path;

    #[test]
    fn index_paths_must_stay_relative_and_contained() {
        assert!(safe_index_path("src/main.rs"));
        assert!(safe_index_path("a/b/c.ts"));
        assert!(!safe_index_path(""));
        assert!(!safe_index_path("/etc/passwd"));
        assert!(!safe_index_path("../outside"));
        assert!(!safe_index_path("a/../../outside"));
        assert!(!safe_index_path("a\\b"));
    }

    #[test]
    fn staleness_header_batches_file_checks() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let conn = super::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, size INTEGER, mtime INTEGER, content_hash INTEGER);",
        )
        .unwrap();
        for (id, rel) in [(1, "Cargo.toml"), (2, "src/query/staleness.rs")] {
            let abs = root.join(rel);
            let meta = std::fs::symlink_metadata(&abs).unwrap();
            let mtime = super::db::metadata_mtime(&meta).unwrap();
            let size = meta.len() as i64;
            let hash = super::fnv1a64(&std::fs::read(&abs).unwrap()) as i64;
            conn.execute(
                "INSERT INTO files VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, rel, size, mtime, hash],
            )
            .unwrap();
        }

        let ctx = super::QueryCtx { conn: &conn, root };
        let set = super::BTreeSet::from([
            "Cargo.toml".to_string(),
            "src/query/staleness.rs".to_string(),
        ]);
        let (header, stale) = super::staleness_header(&ctx, &set);
        assert!(!stale);
        assert!(header.is_none());

        conn.execute(
            "UPDATE files SET size = size + 1 WHERE path = 'src/query/staleness.rs'",
            [],
        )
        .unwrap();
        let (header, stale) = super::staleness_header(&ctx, &set);
        assert!(stale);
        assert!(header.unwrap().contains("1 of 2 result files changed"));
    }
}
