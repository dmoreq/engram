//! Per-repository index facts for audit. Non-aborting: one bad index becomes a
//! check result, never an exit that hides the rest of the report.

use std::path::{Path, PathBuf};

use crate::db;

pub struct RepoFacts {
    pub path: PathBuf,
    pub alias: String,
    pub state: IndexState,
}

pub enum IndexState {
    Ok(IndexFacts),
    Empty(IndexFacts),
    Missing,
    Symlink,
    Tracked,
    Corrupt,
    Schema,
    TooLarge,
    BuildInProgress,
}

pub struct IndexFacts {
    pub files: i64,
    pub symbols: i64,
    pub parse_errors: i64,
    pub stale_ratio: f64,
    pub db_bytes: u64,
    pub built_at: Option<i64>,
}

pub fn collect(paths: &[PathBuf], _now: i64) -> Vec<RepoFacts> {
    if paths.len() <= 1 {
        return paths
            .iter()
            .map(|path| RepoFacts {
                path: path.clone(),
                alias: super::repo_alias(path),
                state: snapshot(path),
            })
            .collect();
    }
    let workers = paths.len().min(4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<(usize, RepoFacts)> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            handles.push(scope.spawn(|| {
                let mut local = Vec::new();
                loop {
                    let idx = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if idx >= paths.len() {
                        break;
                    }
                    let path = &paths[idx];
                    local.push((
                        idx,
                        RepoFacts {
                            path: path.clone(),
                            alias: super::repo_alias(path),
                            state: snapshot(path),
                        },
                    ));
                }
                local
            }));
        }
        let mut all = Vec::with_capacity(paths.len());
        for h in handles {
            if let Ok(part) = h.join() {
                all.extend(part);
            }
        }
        all
    });
    results.sort_by_key(|(idx, _)| *idx);
    results.into_iter().map(|(_, facts)| facts).collect()
}

fn snapshot(root: &Path) -> IndexState {
    if db::reject_tracked_eg_dir(root).is_err() {
        return IndexState::Tracked;
    }
    let conn = match db::try_open_index(root) {
        Ok(conn) => conn,
        Err(db::IndexOpenError::Missing) => return IndexState::Missing,
        Err(db::IndexOpenError::Symlink) => return IndexState::Symlink,
        Err(db::IndexOpenError::Schema { .. }) => return IndexState::Schema,
        Err(db::IndexOpenError::Corrupt) => return IndexState::Corrupt,
    };
    if crate::doctor::build_lock_held(root) {
        return IndexState::BuildInProgress;
    }
    let db_bytes = db::file_size(&db::db_path(root));
    let integrity: std::result::Result<String, _> = if db_bytes > 50 * 1024 * 1024 {
        conn.query_row("PRAGMA quick_check", [], |r| r.get(0))
    } else {
        conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))
    };
    match integrity {
        Ok(v) if v == "ok" => {}
        _ => return IndexState::Corrupt,
    }
    let counts: std::result::Result<(i64, i64, i64), _> = conn.query_row(
        "SELECT COUNT(*), (SELECT COUNT(*) FROM symbols), COALESCE(SUM(parse_status != 'ok'), 0) FROM files",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );
    let Ok((files, symbols, parse_errors)) = counts else {
        return IndexState::Corrupt;
    };
    let built_at = db::meta_get(&conn, db::meta_keys::BUILT_AT).and_then(|v| v.parse::<i64>().ok());
    let mut facts = IndexFacts {
        files,
        symbols,
        parse_errors,
        stale_ratio: 0.0,
        db_bytes,
        built_at,
    };
    if files == 0 {
        return IndexState::Empty(facts);
    }
    if files > 10_000 {
        return IndexState::TooLarge;
    }
    match crate::doctor::stale_ratio_bounded(&conn, root, Some(10_000)) {
        Ok(Some(ratio)) => facts.stale_ratio = ratio,
        Ok(None) => return IndexState::TooLarge,
        Err(_) => return IndexState::Corrupt,
    }
    IndexState::Ok(facts)
}
