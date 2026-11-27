//! Per-pair and scope-level freshness for `eg reuse`. Stats are cheap; a
//! differing mtime triggers a bounded FNV-1a re-hash under one 16 MiB budget
//! per invocation. A missing index row is `unknown`, never silently fresh.

use std::collections::{HashMap, HashSet};

use rusqlite::OptionalExtension;

use super::status::ResolvedScope;
use super::*;

const HASH_BUDGET: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct CheckResult {
    state: Freshness,
    reason: Option<UnknownReason>,
}

const fn cr(state: Freshness, reason: Option<UnknownReason>) -> CheckResult {
    CheckResult { state, reason }
}

struct HashJob {
    path: std::path::PathBuf,
    expected: i64,
}

enum Prepared {
    Ready(CheckResult),
    Hash(HashJob),
}

#[derive(Default)]
pub(crate) struct FreshnessReport {
    pub(super) state: Freshness,
    pub(super) changed_files: usize,
    pub(super) checked_files: usize,
    pub(super) unknown_count: usize,
    pub(super) unknown_reason: Option<UnknownReason>,
}

impl FreshnessReport {
    pub(super) fn headers(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.changed_files > 0 {
            out.push(format!(
                "eg: {} of {} result files changed since build — run eg build",
                self.changed_files, self.checked_files
            ));
        }
        if self.unknown_count > 0 {
            out.push(format!(
                "eg: freshness unknown ({}) — run eg build",
                self.unknown_reason
                    .map(|r| r.as_str())
                    .unwrap_or(UnknownReason::NewFile.as_str())
            ));
        }
        out
    }
}

struct Checker<'a> {
    ctx: &'a QueryCtx<'a>,
    remaining_bytes: u64,
    preloaded: Option<HashMap<String, (i64, i64, i64)>>,
    stmt: Option<rusqlite::Statement<'a>>,
}

impl<'a> Checker<'a> {
    fn new(
        ctx: &'a QueryCtx<'a>,
        preloaded: Option<HashMap<String, (i64, i64, i64)>>,
    ) -> Result<Self> {
        let stmt = if preloaded.is_none() {
            Some(
                ctx.conn
                    .prepare("SELECT mtime, size, content_hash FROM files WHERE path = ?1")?,
            )
        } else {
            None
        };
        Ok(Self {
            ctx,
            remaining_bytes: HASH_BUDGET,
            preloaded,
            stmt,
        })
    }

    #[cfg(test)]
    fn check(&mut self, rel: &str) -> Result<CheckResult> {
        match self.prepare(rel)? {
            Prepared::Ready(result) => Ok(result),
            Prepared::Hash(job) => Ok(hash_job(&job)),
        }
    }

    fn prepare(&mut self, rel: &str) -> Result<Prepared> {
        if !crate::query::staleness::safe_index_path(rel) {
            return Ok(Prepared::Ready(stale(UnknownReason::UnsafePath)));
        }
        let stored = if let Some(map) = &self.preloaded {
            map.get(rel).copied()
        } else if let Some(stmt) = &mut self.stmt {
            stmt.query_row([rel], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .optional()?
        } else {
            None
        };
        let abs = self.ctx.root.join(rel);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Prepared::Ready(if stored.is_some() {
                    stale(UnknownReason::DeletedFile)
                } else {
                    fresh()
                }));
            }
            Err(_) => return Ok(Prepared::Ready(stale(UnknownReason::StatError))),
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Ok(Prepared::Ready(stale(UnknownReason::UnsafePath)));
        }
        let Some((stored_mtime, stored_size, stored_hash)) = stored else {
            return Ok(Prepared::Ready(unknown(UnknownReason::NewFile)));
        };
        Ok(self.check_meta(
            abs,
            meta.len(),
            crate::db::metadata_mtime(&meta),
            (stored_mtime, stored_size, stored_hash),
        ))
    }

    fn check_meta(
        &mut self,
        abs: std::path::PathBuf,
        actual_size: u64,
        actual_mtime: Option<i64>,
        (stored_mtime, stored_size, stored_hash): (i64, i64, i64),
    ) -> Prepared {
        if actual_size != stored_size as u64 {
            return Prepared::Ready(stale_without_reason());
        }
        let Some(mtime) = actual_mtime else {
            return Prepared::Ready(stale(UnknownReason::StatError));
        };
        if mtime == stored_mtime {
            return Prepared::Ready(fresh());
        }
        if self.remaining_bytes < stored_size as u64 {
            return Prepared::Ready(unknown(UnknownReason::HashBudget));
        }
        self.remaining_bytes -= stored_size as u64;
        Prepared::Hash(HashJob {
            path: abs,
            expected: stored_hash,
        })
    }
}

fn preload_files(
    conn: &rusqlite::Connection,
    paths: &[String],
) -> Result<HashMap<String, (i64, i64, i64)>> {
    let mut map = HashMap::with_capacity(paths.len());
    let refs: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|p| *p != "<proposal>")
        .collect();
    if refs.is_empty() {
        return Ok(map);
    }
    crate::db::for_each_chunk(conn, &refs, |conn, placeholders, chunk| {
        let sql = format!(
            "SELECT path, mtime, size, content_hash FROM files WHERE path IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(chunk.iter()))?;
        while let Some(r) = rows.next()? {
            map.insert(r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?));
        }
        Ok(())
    })?;
    Ok(map)
}

fn hash_job(job: &HashJob) -> CheckResult {
    let Ok(bytes) = crate::walk::read_bounded(&job.path) else {
        return stale(UnknownReason::StatError);
    };
    if crate::hash::fnv1a64(&bytes) as i64 == job.expected {
        fresh()
    } else {
        stale_without_reason()
    }
}

fn fresh() -> CheckResult {
    cr(Freshness::Fresh, None)
}
fn stale(reason: UnknownReason) -> CheckResult {
    cr(Freshness::Stale, Some(reason))
}
fn stale_without_reason() -> CheckResult {
    cr(Freshness::Stale, None)
}
fn unknown(reason: UnknownReason) -> CheckResult {
    cr(Freshness::Unknown, Some(reason))
}

fn reason_rank(reason: UnknownReason) -> u8 {
    match reason {
        UnknownReason::WalkError => 5,
        UnknownReason::NewFile => 4,
        UnknownReason::HashBudget => 3,
        UnknownReason::UnsafePath => 2,
        UnknownReason::StatError => 1,
        UnknownReason::DeletedFile => 0,
    }
}

/// Check the pool (or the empty scope, including a new-file walk), then apply
/// pair aggregates and reasons to each candidate.
pub(super) fn evaluate(
    ctx: &QueryCtx<'_>,
    scope: &ResolvedScope,
    candidates: &mut [ReuseCandidate],
) -> Result<FreshnessReport> {
    let (files, preloaded) = if candidates.is_empty() && scope.is_repo() {
        let mut stmt = ctx
            .conn
            .prepare("SELECT path, mtime, size, content_hash FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut paths = Vec::new();
        let mut map = HashMap::new();
        for row in rows {
            let (path, mtime, size, hash) = row?;
            paths.push(path.clone());
            map.insert(path, (mtime, size, hash));
        }
        (paths, map)
    } else if candidates.is_empty() {
        let map = preload_files(ctx.conn, &scope.files)?;
        (scope.files.clone(), map)
    } else {
        let paths = candidate_files(candidates);
        let map = preload_files(ctx.conn, &paths)?;
        (paths, map)
    };
    let walk_stats = if candidates.is_empty()
        && matches!(scope.kind, status::ScopeKind::Repo | status::ScopeKind::Dir)
    {
        Some(crate::walk::walk(ctx.root, &crate::walk::source_exts()))
    } else {
        None
    };
    let walked_map: HashMap<String, &crate::walk::WalkedFile> = match &walk_stats {
        Some(Ok(stats)) => stats.files.iter().map(|f| (f.rel.clone(), f)).collect(),
        _ => HashMap::new(),
    };
    let mut checker = Checker::new(ctx, Some(preloaded))?;
    let mut results: HashMap<String, CheckResult> = HashMap::new();
    let mut seen = HashSet::new();
    for file in files {
        // Proposal candidates have no backing file; stat-ing the sentinel
        // path fails on Windows (InvalidInput) and misreports elsewhere.
        if file == "<proposal>" {
            continue;
        }
        if !seen.insert(file.clone()) {
            continue;
        }
        let prepared = if let (Some(wf), Some(stored)) = (
            walked_map.get(&file),
            checker
                .preloaded
                .as_ref()
                .and_then(|m| m.get(&file).copied()),
        ) {
            if !crate::query::staleness::safe_index_path(&file) {
                Prepared::Ready(stale(UnknownReason::UnsafePath))
            } else {
                checker.check_meta(wf.abs.clone(), wf.size, Some(wf.mtime), stored)
            }
        } else {
            checker.prepare(&file)?
        };
        let res = match prepared {
            Prepared::Ready(result) => result,
            Prepared::Hash(job) => hash_job(&job),
        };
        results.insert(file, res);
    }

    let mut report = FreshnessReport::default();
    if let Some(stats_res) = walk_stats {
        let pre = checker.preloaded.as_ref();
        if let Some(reason) = new_file_walk(ctx, scope, pre, &results, stats_res, &mut report)? {
            if report
                .unknown_reason
                .is_none_or(|c| reason_rank(reason) > reason_rank(c))
            {
                report.unknown_reason = Some(reason);
            }
        }
    }

    for result in results.values() {
        match result.state {
            Freshness::Fresh => report.checked_files += 1,
            Freshness::Stale => {
                report.checked_files += 1;
                report.changed_files += 1;
            }
            Freshness::Unknown => {
                report.unknown_count += 1;
                if result.reason.is_some_and(|r| {
                    report
                        .unknown_reason
                        .is_none_or(|cur| reason_rank(r) > reason_rank(cur))
                }) {
                    report.unknown_reason = result.reason;
                }
            }
        }
    }
    report.state = if report.unknown_count > 0 {
        Freshness::Unknown
    } else if report.changed_files > 0 {
        Freshness::Stale
    } else {
        Freshness::Fresh
    };

    for candidate in candidates.iter_mut() {
        // The CANONICAL file is the evidence: it is the implementation the
        // agent is told to reuse, so its staleness or unknown state must
        // demote the candidate. The target file is what the agent is editing
        // right now — a post-edit review always finds it newer than the
        // index, and demoting on it would sink every honest review result.
        // Target-side changes still surface through the report header.
        let canonical_result = results.get(&candidate.canonical_file);
        if let Some(result) = canonical_result {
            if result.reason == Some(UnknownReason::UnsafePath) {
                push_reason(candidate, ReasonCode::UnsafePath);
            }
            match result.state {
                Freshness::Unknown => push_reason(candidate, ReasonCode::FreshnessUnknown),
                Freshness::Stale => push_reason(candidate, ReasonCode::StaleIndex),
                Freshness::Fresh => {}
            }
        }
        let canonical_state = canonical_result
            .map(|r| r.state)
            .unwrap_or(Freshness::Fresh);
        if canonical_state != Freshness::Fresh {
            // Stale or unknown canonical evidence never carries Medium/High
            // authority.
            candidate.confidence = ReuseConfidence::Low;
        }
    }
    Ok(report)
}

fn push_reason(candidate: &mut ReuseCandidate, reason: ReasonCode) {
    if !candidate.reasons.contains(&reason) {
        candidate.reasons.push(reason);
        candidate.reasons.sort_by_key(|r| r.as_str());
    }
}

fn candidate_files(candidates: &[ReuseCandidate]) -> Vec<String> {
    let mut files = HashSet::new();
    for candidate in candidates {
        files.insert(candidate.file.clone());
        files.insert(candidate.canonical_file.clone());
    }
    let mut out: Vec<String> = files.into_iter().collect();
    out.sort();
    out
}

/// A source file on disk but absent from the index is `unknown/new-file`.
/// The walk is scope-limited and only runs for an empty candidate pool.
fn new_file_walk(
    ctx: &QueryCtx<'_>,
    scope: &ResolvedScope,
    preloaded: Option<&HashMap<String, (i64, i64, i64)>>,
    results: &HashMap<String, CheckResult>,
    walk_res: Result<crate::walk::WalkStats>,
    report: &mut FreshnessReport,
) -> Result<Option<UnknownReason>> {
    let owned_indexed: Option<HashSet<String>> = if scope.is_repo() && preloaded.is_some() {
        None
    } else {
        let mut stmt = ctx.conn.prepare("SELECT path FROM files")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Some(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
    };
    let stats = match walk_res {
        Ok(stats) => stats,
        Err(_) => return Ok(Some(UnknownReason::WalkError)),
    };
    let mut unknown_files = HashSet::new();
    for file in stats.files {
        let rel = file.rel;
        let is_indexed = match (&owned_indexed, preloaded) {
            (Some(set), _) => set.contains(&rel),
            (None, Some(map)) => map.contains_key(&rel),
            (None, None) => false,
        };
        if !in_scope(&rel, scope)
            || is_indexed
            || rel.starts_with(".git/")
            || rel.starts_with(".engram/")
            || rel.starts_with(".tmp/")
            || rel.starts_with("target/")
            || results.contains_key(&rel)
        {
            continue;
        }
        unknown_files.insert(rel);
    }
    if unknown_files.is_empty() {
        return Ok(None);
    }
    report.unknown_count += unknown_files.len();
    Ok(Some(UnknownReason::NewFile))
}

fn in_scope(path: &str, scope: &ResolvedScope) -> bool {
    scope.is_repo() || super::target::path_matches(path, &scope.paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> &'static std::path::Path {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn hash_budget_returns_unknown_instead_of_stale() {
        let root = repo_root();
        let size = std::fs::metadata(root.join("Cargo.toml")).unwrap().len() as i64;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (path TEXT, mtime INTEGER, size INTEGER, content_hash INTEGER);",
        )
        .unwrap();
        conn.execute("INSERT INTO files VALUES ('Cargo.toml', 0, ?1, 0)", [size])
            .unwrap();
        let ctx = QueryCtx { conn: &conn, root };
        let mut checker = Checker::new(&ctx, None).unwrap();
        checker.remaining_bytes = 0;
        let result = checker.check("Cargo.toml").unwrap();
        assert_eq!(result.state, Freshness::Unknown);
        assert_eq!(result.reason, Some(UnknownReason::HashBudget));
    }

    #[test]
    fn missing_files_row_but_existing_file_is_new_file() {
        let root = repo_root();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (path TEXT, mtime INTEGER, size INTEGER, content_hash INTEGER);",
        )
        .unwrap();
        let ctx = QueryCtx { conn: &conn, root };
        let mut checker = Checker::new(&ctx, None).unwrap();
        let result = checker.check("Cargo.toml").unwrap();
        assert_eq!(result.state, Freshness::Unknown);
        assert_eq!(result.reason, Some(UnknownReason::NewFile));
    }
}
