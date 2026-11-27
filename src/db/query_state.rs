//! Incremental-build state queries: previous-index probing, schema
//! compatibility, carried-symbol loading, and gap detection for changed names.

use super::*;

/// Fast path gate: usable previous index under the same schema & grammar versions.
pub fn probe_old(root: &Path) -> Result<Option<Connection>> {
    reject_symlink(&eg_dir(root), ".engram directory")?;
    let old_path = db_path(root);
    reject_symlink(&old_path, "index.db")?;
    if !old_path.exists() {
        return Ok(None);
    }
    let Ok(old) = Connection::open_with_flags(&old_path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Ok(None);
    };
    Ok(schema_compatible(&old).then_some(old))
}

fn schema_compatible(old: &Connection) -> bool {
    let Ok(SCHEMA_VERSION) = old.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
    else {
        return false;
    };
    let req = [
        ("symbols", "file_path"),
        ("symbols", "is_test"),
        ("unresolved_calls", "site_line"),
        ("unresolved_calls", "kind"),
    ];
    req.iter()
        .all(|(t, c)| column_present(old, t, c).unwrap_or(false))
        && object_exists(old, "table", "symbols_fts")
        && meta_get(old, meta_keys::GRAMMAR_VERSION).as_deref() == Some(GRAMMAR_VERSION)
        && meta_get(old, meta_keys::SIGNATURE_VERSION).as_deref() == Some(SIGNATURE_VERSION)
        && meta_get(old, meta_keys::METRICS_VERSION).as_deref() == Some(METRICS_VERSION)
}

/// Fallible `sqlite_master` probe: distinguishes "absent" from "probe failed".
pub(super) fn object_exists_result(
    conn: &Connection,
    kind: &str,
    name: &str,
) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
        rusqlite::params![kind, name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// True when `sqlite_master` holds an object of `kind` named `name`.
fn object_exists(conn: &Connection, kind: &str, name: &str) -> bool {
    object_exists_result(conn, kind, name).unwrap_or(false)
}

/// True when `table` has a column named `column`. Callers that only need a
/// boolean gate pair this with `.unwrap_or(false)` to swallow read errors.
pub(crate) fn column_present(
    conn: &Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        rusqlite::params![table, column],
        |r| r.get(0),
    )?;
    Ok(present > 0)
}

/// Stat fingerprint of one previous-index file row: (mtime, size,
/// content_hash). Named so a column reorder in the SELECT cannot silently
/// swap a hash for an mtime on the fast path.
pub struct FileStat {
    pub mtime: i64,
    pub size: i64,
    pub content_hash: i64,
}

/// (rel -> stat fingerprint) of the previous index's files.
pub fn old_files(old: &Connection) -> Result<HashMap<String, FileStat>> {
    let mut stmt = old.prepare("SELECT path, mtime, size, content_hash FROM files")?;
    let rows = stmt
        .query_map(rusqlite::params![], |r| {
            Ok((
                r.get::<_, String>(0)?,
                FileStat {
                    mtime: r.get(1)?,
                    size: r.get(2)?,
                    content_hash: r.get(3)?,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect())
}

/// Count parse-error files among the carried set (fast-path gate denominator).
/// One chunked IN query instead of one query per file.
pub fn old_error_count(old: &Connection, rels: &[String]) -> Result<usize> {
    let mut n = 0usize;
    for_each_chunk(old, rels, |old, placeholders, chunk| {
        let sql = format!(
            "SELECT COUNT(*) FROM files WHERE parse_status != 'ok' AND path IN ({placeholders})"
        );
        let c: i64 = old.query_row(&sql, rusqlite::params_from_iter(chunk.iter()), |r| r.get(0))?;
        n += c as usize;
        Ok(())
    })?;
    Ok(n)
}

/// Transitive dirty closure over the previous index (fast-path correctness):
/// every file with an edge (any kind, resolved destination) into a changed
/// file must be re-extracted, or its copied edge dies with the old destination
/// id — and any file calling into a re-extracted file needs the same treatment.
/// Returns changed + closure; the caller moves the carried members to `fresh`.
pub fn dirty_closure(old: &Connection, changed: &[String]) -> Result<HashSet<String>> {
    // Work-queue (R6f Blocker 4): each round queries only the NEWLY dirtied
    // paths, not the whole accumulated set. The old fixed-point re-queried
    // everything each round — O(N^2) on long dependency chains.
    let mut dirty: HashSet<String> = changed.iter().cloned().collect();
    let mut frontier: Vec<String> = changed.to_vec();
    while !frontier.is_empty() {
        frontier.sort();
        let mut new_paths: HashSet<String> = HashSet::new();
        for_each_chunk(old, &frontier, |old, placeholders, chunk| {
            let sql = format!(
                "SELECT DISTINCT f.path
                 FROM edges e
                 {EDGE_SRC_FILE_JOIN}
                 JOIN symbols ds ON ds.id = e.dst_id JOIN files df ON df.id = ds.file_id
                 WHERE df.path IN ({placeholders})"
            );
            collect_gap_paths(old, &sql, chunk, &mut new_paths)
        })?;
        // Work-queue guard: already-dirty paths never re-enter the frontier,
        // so the BFS terminates on dependency cycles too.
        new_paths.retain(|p| !dirty.contains(p));
        let next: Vec<String> = new_paths.into_iter().collect();
        dirty.extend(next.iter().cloned());
        frontier = next;
    }
    Ok(dirty)
}

/// Old symbol qualnames grouped by the files that declare them. Used to detect
/// a newly added duplicate candidate, not just a brand-new name.
pub fn symbol_qualname_files(conn: &Connection) -> Result<HashMap<String, HashSet<String>>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stmt =
        conn.prepare("SELECT s.qualname, f.path FROM symbols s JOIN files f ON f.id = s.file_id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for r in rows {
        let (qualname, path) = r?;
        out.entry(qualname).or_default().insert(path);
    }
    Ok(out)
}

/// Qualnames declared by the given old-index files. Bounds candidate
/// invalidation to the files that actually changed.
pub fn qualnames_for_files(old: &Connection, files: &[String]) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for_each_chunk(old, files, |old, placeholders, chunk| {
        let sql = format!(
            "SELECT DISTINCT s.qualname
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path IN ({placeholders})"
        );
        collect_gap_paths(old, &sql, chunk, &mut out)
    })?;
    Ok(out)
}

/// Carried files whose edges or unresolved sites mention a qualname whose
/// candidate set changed (grew, shrank, or was renamed). Those sites may now
/// be ambiguous or resolve to a different candidate; re-extract them so the
/// incremental result matches a fresh build.
pub fn gap_files_for_candidate_changes(
    old: &Connection,
    changed_qualnames: &HashSet<String>,
) -> Result<HashSet<String>> {
    if changed_qualnames.is_empty() {
        return Ok(HashSet::new());
    }
    let names: Vec<String> = changed_qualnames.iter().cloned().collect();
    let mut out = HashSet::new();
    for_each_chunk(old, &names, |old, placeholders, chunk| {
        let edge_sql = format!(
            "SELECT DISTINCT f.path
             FROM edges e
             JOIN symbols s ON s.id = e.src_id
             JOIN files f ON f.id = s.file_id
             WHERE e.kind IN {kinds} AND e.dst_name IN ({placeholders})",
            kinds = edge_kinds_sql()
        );
        collect_gap_paths(old, &edge_sql, chunk, &mut out)?;
        let unresolved_sql = format!(
            "SELECT DISTINCT f.path
             FROM unresolved_calls u JOIN files f ON f.id = u.file_id
             WHERE u.dst_name IN ({placeholders})"
        );
        collect_gap_paths(old, &unresolved_sql, chunk, &mut out)?;
        Ok(())
    })?;
    Ok(out)
}

/// Escape the three GLOB metacharacters into bracket classes so a bound
/// segment matches itself literally (`_` and `%` are not special in GLOB).
fn glob_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '*' | '?' | '[') {
            out.push('[');
            out.push(c);
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

/// Carried files whose resolution gaps mention any of `new_names`
/// (Amendment 15): a file with an unresolved call or an AMBIGUOUS calls edge
/// whose last name segment is a newly added symbol must be re-extracted, or
/// the fast path would keep a stale unresolved answer. Bounded by the gap
/// rows, not by the corpus.
pub fn gap_files_for_new_names(
    old: &Connection,
    new_qualnames: &HashSet<String>,
) -> Result<HashSet<String>> {
    if new_qualnames.is_empty() {
        return Ok(HashSet::new());
    }
    // `SQLITE_MAX_EXPR_DEPTH` is 1000; 100 segments * 2 OR terms stays safely below it.
    let mut segments: Vec<String> = new_qualnames
        .iter()
        .map(|q| glob_literal(q.rsplit('.').next().unwrap_or(q)))
        .collect();
    segments.sort_unstable();
    segments.dedup();
    const GLOB_OR_CHUNK: usize = 100;
    let mut out = HashSet::new();
    for chunk in segments.chunks(GLOB_OR_CHUNK) {
        let last_segment = (1..=chunk.len())
            .map(|n| format!("dst_name GLOB ?{n} OR dst_name GLOB '*.' || ?{n}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let edge_sql = format!(
            "SELECT DISTINCT f.path FROM edges e JOIN symbols s ON s.id = e.src_id JOIN files f ON f.id = s.file_id
             WHERE e.kind IN {kinds} AND e.confidence = '{ambiguous}' AND ({last_segment})",
            kinds = edge_kinds_sql(), ambiguous = Confidence::Ambiguous.as_str()
        );
        collect_gap_paths(old, &edge_sql, chunk, &mut out)?;
        let unresolved_sql = format!("SELECT DISTINCT f.path FROM unresolved_calls u JOIN files f ON f.id = u.file_id WHERE {last_segment}");
        collect_gap_paths(old, &unresolved_sql, chunk, &mut out)?;
    }
    Ok(out)
}

fn collect_gap_paths(
    conn: &Connection,
    sql: &str,
    chunk: &[String],
    out: &mut HashSet<String>,
) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    for row in stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
        r.get::<_, String>(0)
    })? {
        out.insert(row?);
    }
    Ok(())
}

pub(crate) const EDGE_SRC_FILE_JOIN: &str =
    "JOIN symbols s ON s.id = e.src_id JOIN files f ON f.id = s.file_id";
pub(crate) const CALLER_COUNT_SQL: &str = "(SELECT COUNT(DISTINCT e.src_id) FROM edges e JOIN symbols src ON src.id = e.src_id WHERE e.dst_id = s.id AND e.kind = 'calls' AND e.src_id != s.id AND src.is_test = s.is_test)";
pub(crate) fn edge_kind_sql(kinds: &[EdgeKind]) -> String {
    kinds
        .iter()
        .map(|k| format!("'{}'", k.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}
pub(crate) fn edge_kinds_sql() -> String {
    format!(
        "({})",
        edge_kind_sql(&[
            EdgeKind::Calls,
            EdgeKind::Imports,
            EdgeKind::Inherits,
            EdgeKind::TypeRef
        ])
    )
}

/// Ref freshness verdict for one `--refs` token: `Valid` names an indexed
/// entity no newer than the note's anchor, `Modified` names one that changed
/// after it, and `Broken` names nothing resolvable. Lives in the db layer so
/// both `mem query` (service) and the reuse allowlist (query) can call it
/// downward instead of one of them reaching sideways or upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefStatus {
    Valid,
    Modified,
    Broken,
    Unverified,
}

impl RefStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Modified => "modified",
            Self::Broken => "broken",
            Self::Unverified => "unverified",
        }
    }
    pub fn is_verified(self) -> bool {
        matches!(self, Self::Valid | Self::Modified)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRef {
    pub status: RefStatus,
    pub kind: &'static str,
    pub detail: String,
    pub mtime_secs: Option<i64>,
}

impl ResolvedRef {
    /// Re-evaluate freshness against a different note `anchor` without re-querying.
    pub fn with_anchor(&self, anchor: i64) -> Self {
        let status = match (self.status, self.mtime_secs) {
            (RefStatus::Valid | RefStatus::Modified, Some(mtime)) => {
                if mtime > anchor {
                    RefStatus::Modified
                } else {
                    RefStatus::Valid
                }
            }
            _ => self.status,
        };
        Self {
            status,
            kind: self.kind,
            detail: self.detail.clone(),
            mtime_secs: self.mtime_secs,
        }
    }
}

/// Check whether `r` is a safe repo-relative regular file under `root` (not
/// a symlink, contained in `root`), returning `(size_bytes, mtime_secs)`.
pub(crate) fn check_disk_ref(root: &Path, r: &str) -> Option<(u64, i64)> {
    let rel = Path::new(r);
    if r.is_empty()
        || r.contains('\\')
        || rel.is_absolute()
        || !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    let abs = root.join(rel);
    let meta = std::fs::symlink_metadata(&abs).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return None;
    }
    let canon_root = std::fs::canonicalize(root).ok()?;
    let canon_abs = std::fs::canonicalize(&abs).ok()?;
    if !canon_abs.starts_with(&canon_root) {
        return None;
    }
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((meta.len(), mtime_secs))
}

/// Resolve one `--refs` token against the index and compare its newest
/// indexed mtime against the note's `anchor`. File-shaped tokens match the
/// exact path first, then a same-suffix path (a moved `a/b.rs` still resolves
/// through `x/a/b.rs`); anything else resolves as a symbol qualname or name.
pub(crate) fn check_ref_status(
    index_conn: &rusqlite::Connection,
    r: &str,
    anchor: i64,
) -> RefStatus {
    check_ref_status_with_root(index_conn, None, r, anchor)
}

/// Resolve one `--refs` token against `index_conn` and optional `root`,
/// returning status, kind (`"file"`, `"symbol"`, `"path"`), and location detail.
pub fn resolve_ref_detail(
    index_conn: Option<&Connection>,
    root: Option<&Path>,
    r: &str,
    anchor: i64,
) -> ResolvedRef {
    let from_mtime = |kind: &'static str, detail: String, mtime_secs: i64| ResolvedRef {
        status: if mtime_secs > anchor {
            RefStatus::Modified
        } else {
            RefStatus::Valid
        },
        kind,
        detail,
        mtime_secs: Some(mtime_secs),
    };
    let Some(iconn) = index_conn else {
        if let Some((size, mtime_secs)) = root.and_then(|repo_root| check_disk_ref(repo_root, r)) {
            return from_mtime("file", format!("{r} ({size} bytes)"), mtime_secs);
        }
        return ResolvedRef {
            status: RefStatus::Unverified,
            kind: "path",
            detail: String::new(),
            mtime_secs: None,
        };
    };
    let query_file = |sql: &str, param: &str| -> Option<(String, i64, i64)> {
        iconn
            .prepare_cached(sql)
            .ok()?
            .query_row(rusqlite::params![param], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .ok()
    };
    if let Some((path, size, mtime_nanos)) = query_file("SELECT path, size, mtime FROM files WHERE path = ?1 LIMIT 1", r)
        .or_else(|| query_file("SELECT path, size, mtime FROM files WHERE path LIKE ?1 ESCAPE '\\' ORDER BY mtime DESC, path ASC LIMIT 1", &format!("%/{}", like_escape(r))))
    {
        return from_mtime("file", format!("{path} ({size} bytes)"), mtime_nanos / 1_000_000_000);
    }
    if let Ok(mut stmt) = iconn.prepare_cached(
        "SELECT f.path, s.line_start, s.line_end, f.mtime FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.qualname = ?1 OR s.name = ?1 ORDER BY f.mtime DESC, s.qualname, s.file_path LIMIT 1",
    ) {
        if let Ok((fp, ls, le, mtime_nanos)) = stmt.query_row(rusqlite::params![r], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
        }) {
            return from_mtime("symbol", format!("{fp}:{ls}-{le}"), mtime_nanos / 1_000_000_000);
        }
    }
    if let Some((size, mtime_secs)) = root.and_then(|repo_root| check_disk_ref(repo_root, r)) {
        return from_mtime("file", format!("{r} ({size} bytes)"), mtime_secs);
    }
    ResolvedRef {
        status: RefStatus::Broken,
        kind: "path",
        detail: String::new(),
        mtime_secs: None,
    }
}

/// Like [`check_ref_status`], but falls back to an on-disk regular-file check
/// under `root` when `index.db` `files` and `symbols` lookups miss.
pub(crate) fn check_ref_status_with_root(
    index_conn: &rusqlite::Connection,
    root: Option<&Path>,
    r: &str,
    anchor: i64,
) -> RefStatus {
    resolve_ref_detail(Some(index_conn), root, r, anchor).status
}
