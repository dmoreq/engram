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
        ("symbols", "return_type"),
        ("unresolved_calls", "site_line"),
        ("unresolved_calls", "kind"),
    ];
    req.iter()
        .all(|(t, c)| column_present(old, t, c).unwrap_or(false))
        && object_exists(old, "table", "symbols_fts")
        && object_exists(old, "table", "class_fields")
        && object_exists(old, "table", "module_exports")
        && object_exists(old, "table", "module_stars")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefFailureKind {
    MissingCodePath,
    MissingDocOrReport,
    EphemeralTmpPath,
    ExternalAbsPath,
    MissingSymbol,
    SymlinkOrEscape,
}

impl RefFailureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingCodePath => "missing_code_path",
            Self::MissingDocOrReport => "missing_doc_or_report",
            Self::EphemeralTmpPath => "ephemeral_tmp_path",
            Self::ExternalAbsPath => "external_abs_path",
            Self::MissingSymbol => "missing_symbol",
            Self::SymlinkOrEscape => "symlink_or_escape",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRef {
    pub status: RefStatus,
    pub kind: &'static str,
    pub detail: String,
    pub mtime_secs: Option<i64>,
    pub failure_kind: Option<RefFailureKind>,
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
            failure_kind: self.failure_kind,
        }
    }
}

mod refs;
#[allow(unused_imports)]
pub(crate) use refs::{
    check_disk_ref, check_ref_status, check_ref_status_with_root, lookup_symbol_ref,
    split_line_suffix,
};
pub use refs::{resolve_ref_detail, resolve_ref_detail_with_canon};

#[cfg(test)]
mod tests;
