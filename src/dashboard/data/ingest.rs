//! Journal ingest for the dashboard panels: repo discovery, the single
//! live-event gate, cached full reads, and the incremental tail. Every line
//! reader here is capped, so a corrupted or hostile writer must not force an
//! unbounded allocation on any poll or read.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Discover candidate repo roots for a dashboard panel into `repos`. The map
/// key is the repository path (journal `repo` values are canonical), so two
/// checkouts sharing a directory name are two entries instead of the second
/// silently displacing the first. An explicit root short-circuits the scan
/// (its entry replaces any same-path one); otherwise every journaled repo
/// with `.engram/<db_rel>` present is added in entry order without
/// displacing existing entries, and the current directory joins last.
/// Display labels come from `display_name_for`, assigned over the sorted key
/// set. Shared by the reuse and memories panels so their store discovery
/// cannot drift.
pub(super) fn discover_repos(
    root: Option<&Path>,
    entries: &[Value],
    db_rel: &str,
    repos: &mut HashMap<String, PathBuf>,
) {
    if let Some(r) = root {
        let db_path = r.join(".engram").join(db_rel);
        if db_path.is_file() {
            repos.insert(r.to_string_lossy().into_owned(), r.to_path_buf());
        }
    } else {
        for e in entries {
            if let Some(r) = e.get("repo").and_then(Value::as_str) {
                // Dedupe before the stat: a long journal re-probing one root
                // pays one stat per distinct repository, not per line.
                if repos.contains_key(r) {
                    continue;
                }
                let p = PathBuf::from(r);
                if p.join(".engram").join(db_rel).is_file() {
                    repos.entry(r.to_string()).or_insert(p);
                }
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            if cwd.join(".engram").join(db_rel).is_file() {
                let key = cwd.to_string_lossy().into_owned();
                repos.entry(key).or_insert(cwd);
            }
        }
    }
}

/// Display label for one repository path: the basename, escalating one
/// ancestor per collision (`parent-dir/basename`, then one more) until unique.
/// Assigned over the sorted path set, so every consumer derives the same
/// label for the same store set.
pub(super) fn display_name_for(
    path: &Path,
    taken: &mut std::collections::HashSet<String>,
) -> String {
    let comps: Vec<String> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .map(str::to_string)
        .collect();
    for depth in 1..=comps.len() {
        let label = comps[comps.len() - depth..].join("/");
        if taken.insert(label.clone()) {
            return label;
        }
    }
    // Every ancestor prefix collided; the full path is unique by construction.
    let full = path.to_string_lossy().into_owned();
    taken.insert(full.clone());
    full
}

/// Drop repos whose `.engram/` is tracked/symlinked; return security errors
/// keyed by each repo's display label.
pub(super) fn retain_untracked(
    repos: &mut HashMap<String, PathBuf>,
    labels: &HashMap<String, String>,
) -> (Vec<Value>, usize) {
    let mut errors = Vec::new();
    let mut blocked = 0usize;
    repos.retain(|path, repo| {
        let gate = crate::db::reject_symlink(&repo.join(".engram"), ".engram directory")
            .and_then(|()| crate::db::reject_tracked_eg_dir(repo));
        match gate {
            Ok(()) => true,
            Err(err) => {
                blocked += 1;
                let name = labels.get(path).map(String::as_str).unwrap_or(path);
                errors.push(serde_json::json!({
                    "repo": name, "stage": "security", "message": err.to_string(),
                }));
                false
            }
        }
    });
    (errors, blocked)
}

/// One journal line is capped before parsing: a corrupted or hostile writer
/// must not force a multi-megabyte parse on every panel request. The capped
/// reader enforces the same bound at read time; a line over the cap is
/// dropped before parse and is not surfaced in any counter — a documented
/// silent-skip limitation, not an error (observability is a follow-up
/// candidate).
pub(super) const MAX_LINE_BYTES: usize = 256 * 1024;

/// The single choke point every journal reader goes through: `None` when the
/// line is not valid JSON or does not name a live repository — dead roots,
/// identityless legacy lines, and relative test artifacts all fail the same
/// one predicate. `read_usage_entries`, the SSE stream, and the monitor all
/// call it, so nothing without a live repository can reach any panel or feed.
/// The length check stays after the capped reader as defense in depth.
pub fn live_event(trimmed: &str) -> Option<Value> {
    if trimmed.len() > MAX_LINE_BYTES {
        return None;
    }
    let val = serde_json::from_str::<Value>(trimmed).ok()?;
    crate::journal::liveness::repo_is_live(val.get("repo").and_then(Value::as_str)).then_some(val)
}

/// One capped read from a journal reader. Lines over `MAX_LINE_BYTES` are
/// discarded before parse; a torn tail stays unread exactly as `Tail`
/// documents.
pub(super) enum CappedLine {
    /// A newline-terminated line of at most `MAX_LINE_BYTES` content bytes:
    /// the content without its terminator, and the byte count consumed
    /// (newline included).
    Line(String, u64),
    /// A line over the cap, skipped through its terminating newline: the
    /// byte count consumed.
    Skipped(u64),
    /// The stream ended inside a line that still fits: a torn tail, left
    /// unread so the next read sees the whole line once it is complete.
    Torn,
    /// End of stream.
    Eof,
}

/// Read one line bounded by `MAX_LINE_BYTES`: the newline is searched within
/// the cap, so no single journal line can force an unbounded allocation. A
/// fitting line returns whole; an oversized line is discarded through its
/// newline and skipped; a torn tail stays unread exactly as `Tail` documents.
pub(super) fn read_line_capped(mut reader: &mut impl BufRead) -> std::io::Result<CappedLine> {
    let mut raw = Vec::new();
    // The +1 keeps the newline of a line whose content is exactly the cap:
    // the fits boundary is the content length, matching `live_event`.
    let mut limited = Read::take(&mut *reader, (MAX_LINE_BYTES + 1) as u64);
    let n = limited.read_until(b'\n', &mut raw)?;
    if n == 0 {
        return Ok(CappedLine::Eof);
    }
    if raw.last() == Some(&b'\n') {
        let mut len = raw.len() - 1;
        if len > 0 && raw[len - 1] == b'\r' {
            len -= 1;
        }
        let line = String::from_utf8(raw[..len].to_vec())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        return Ok(CappedLine::Line(line, n as u64));
    }
    if n <= MAX_LINE_BYTES {
        // The stream ended inside a line that still fits: a torn tail.
        return Ok(CappedLine::Torn);
    }
    let mut skipped = n as u64;
    skipped += skip_through_newline(&mut reader)?;
    Ok(CappedLine::Skipped(skipped))
}

/// Consume bytes through the next newline (or EOF): the bounded discard path
/// for an oversized line. Generations are rotation-capped at 5 MiB, so the
/// scan stays bounded in practice.
fn skip_through_newline(reader: &mut impl BufRead) -> std::io::Result<u64> {
    let mut total = 0u64;
    loop {
        let (consumed, terminated) = {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                return Ok(total);
            }
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (buf.len(), false),
            }
        };
        reader.consume(consumed);
        total += consumed as u64;
        if terminated {
            return Ok(total);
        }
    }
}

fn generation_stamp(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (mtime, meta.len())
        }
        Err(_) => (0, 0),
    }
}

/// Cached parse keyed by the log path plus both generations' (mtime, length)
/// stamps; the path disambiguates stores with identical stamps.
fn read_entries_cached(path: &Path) -> Vec<Value> {
    use std::sync::{Mutex, OnceLock};
    struct EntryCache {
        log_path: PathBuf,
        generations: ((u64, u64), (u64, u64)),
        entries: Vec<Value>,
    }
    static CACHE: OnceLock<Mutex<Option<EntryCache>>> = OnceLock::new();
    let cell = CACHE.get_or_init(|| Mutex::new(None));
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let rotated = parent.join("usage.log.1");
    let generations = (generation_stamp(&rotated), generation_stamp(path));
    let fresh = |c: &EntryCache| c.log_path == path && c.generations == generations;
    if let Ok(guard) = cell.lock() {
        if let Some(cached) = guard.as_ref().filter(|c| fresh(c)) {
            return cached.entries.clone();
        }
    }
    let entries = read_usage_entries_uncached(path);
    // Re-check: a write during the parse would pin a stale-but-matching stamp.
    let after = (generation_stamp(&rotated), generation_stamp(path));
    if after == generations {
        if let Ok(mut guard) = cell.lock() {
            *guard = Some(EntryCache {
                log_path: path.to_path_buf(),
                generations,
                entries: entries.clone(),
            });
        }
    }
    entries
}

pub fn read_usage_entries(log_path: &Path) -> Vec<Value> {
    if std::env::var("ENGRAM_DASHBOARD_NO_CACHE").as_deref() == Ok("1") {
        return read_usage_entries_uncached(log_path);
    }
    read_entries_cached(log_path)
}

pub(super) fn read_usage_entries_uncached(log_path: &Path) -> Vec<Value> {
    let mut entries = Vec::new();
    let parent = log_path.parent().unwrap_or_else(|| Path::new("."));

    for filename in &["usage.log.1", "usage.log"] {
        let p = parent.join(filename);
        if let Ok(file) = File::open(&p) {
            let mut reader = BufReader::new(file);
            loop {
                match read_line_capped(&mut reader) {
                    Ok(CappedLine::Line(line, _)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(val) = live_event(trimmed) {
                            entries.push(val);
                        }
                    }
                    Ok(CappedLine::Skipped(_)) => {}
                    // A torn tail or the end of the file stops this generation.
                    Ok(_) | Err(_) => break,
                }
            }
        }
    }
    entries
}

/// One incremental journal tail shared by the SSE stream and both monitor
/// paths. Tracks file identity (device, inode) so a rotation or an in-place
/// prune is a `Reset`, never a silent replay: the caller reloads its state
/// instead of re-counting lines it already served.
pub struct Tail {
    pos: u64,
    identity: Option<(u64, u64)>,
}

/// One poll result: a complete journal line, or a reset signal.
pub enum TailEvent<'a> {
    Line(&'a str),
    Reset,
}

fn file_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        // volume_serial_number + file_index survive appends; the old
        // (0, len) identity fired a Reset on EVERY append because the length
        // changed with each line. Unavailable metadata falls back to a
        // constant identity: appends stream, never reset.
        use std::os::windows::fs::MetadataExt;
        match (meta.volume_serial_number(), meta.file_index()) {
            (Some(vol), Some(idx)) => (u64::from(vol), u64::from(idx)),
            _ => (0, 0),
        }
    }
}

impl Tail {
    /// Start at the current end of the journal: only new lines are delivered.
    pub fn new(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) => Self {
                pos: meta.len(),
                identity: Some(file_identity(&meta)),
            },
            Err(_) => Self {
                pos: 0,
                identity: None,
            },
        }
    }

    /// Start at an explicit byte offset (tests and replay-on-demand paths).
    pub fn starting_at(path: &Path, pos: u64) -> Self {
        let identity = std::fs::metadata(path).ok().map(|m| file_identity(&m));
        Self { pos, identity }
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Deliver every complete new line. A shrink or identity change yields one
    /// `Reset` and advances to the new end (no replay). A torn trailing line
    /// stays unread until its newline arrives; a line over the read cap is
    /// skipped whole, and the offset lands past it.
    pub fn poll(&mut self, path: &Path, mut on: impl FnMut(TailEvent<'_>)) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let id = file_identity(&meta);
        if self.identity != Some(id) || meta.len() < self.pos {
            on(TailEvent::Reset);
            self.pos = meta.len();
            self.identity = Some(id);
            return;
        }
        if meta.len() == self.pos {
            return;
        }
        let Ok(file) = File::open(path) else {
            return;
        };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.pos)).is_err() {
            return;
        }
        let mut start = self.pos;
        loop {
            match read_line_capped(&mut reader) {
                Ok(CappedLine::Line(line, consumed)) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        on(TailEvent::Line(trimmed));
                    }
                    start += consumed;
                    self.pos = start;
                }
                Ok(CappedLine::Skipped(consumed)) => {
                    start += consumed;
                    self.pos = start;
                }
                // A torn tail stays unread: the offset does not move past it.
                Ok(_) | Err(_) => break,
            }
        }
    }
}
