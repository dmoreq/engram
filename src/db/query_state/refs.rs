use super::*;

/// Strip an optional `:line` or `:start-end` suffix (verifying numeric range).
/// Leaves Rust `::` paths untouched.
pub(crate) fn split_line_suffix(r: &str) -> &str {
    let Some((prefix, suffix)) = r.rsplit_once(':') else {
        return r;
    };
    if prefix.ends_with(':') || suffix.is_empty() {
        return r;
    }
    if let Some((a, b)) = suffix.split_once('-') {
        if let (Ok(start), Ok(end)) = (a.parse::<u64>(), b.parse::<u64>()) {
            if start >= 1 && end >= start {
                return prefix;
            }
        }
        return r;
    }
    if suffix.parse::<u64>().is_ok_and(|line| line >= 1) {
        return prefix;
    }
    r
}

fn has_known_file_ext(path: &str) -> bool {
    let Some((_, ext)) = path.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "rs" | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "md"
            | "txt"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "sql"
            | "sh"
            | "html"
            | "css"
            | "lock"
            | "c"
            | "h"
            | "cpp"
            | "go"
            | "java"
            | "rb"
            | "kt"
            | "swift"
    )
}

fn check_disk_ref_inner(
    root: &Path,
    canon_root: Option<&Path>,
    r: &str,
) -> Option<(u64, i64, bool)> {
    let clean = split_line_suffix(r);
    let rel = Path::new(clean);
    if clean.is_empty()
        || clean.contains('\\')
        || rel.is_absolute()
        || !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    let abs = root.join(rel);
    let meta = std::fs::symlink_metadata(&abs).ok()?;
    let is_dir = meta.is_dir();
    if meta.file_type().is_symlink() || (!meta.is_file() && !is_dir) {
        return None;
    }
    let owned_canon;
    let root_canon = match canon_root {
        Some(c) => c,
        None => {
            owned_canon = std::fs::canonicalize(root).ok()?;
            &owned_canon
        }
    };
    let canon_abs = std::fs::canonicalize(&abs).ok()?;
    if !canon_abs.starts_with(root_canon) {
        return None;
    }
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((meta.len(), mtime_secs, is_dir))
}

/// Check whether `r` is a safe repo-relative file or directory under `root`
/// (not a symlink, contained in `root`), returning `(size_bytes, mtime_secs)`.
#[allow(dead_code)]
pub(crate) fn check_disk_ref(root: &Path, r: &str) -> Option<(u64, i64)> {
    check_disk_ref_inner(root, None, r).map(|(size, mtime, _)| (size, mtime))
}

pub(crate) fn check_ref_status(
    index_conn: &rusqlite::Connection,
    r: &str,
    anchor: i64,
) -> RefStatus {
    check_ref_status_with_root(index_conn, None, r, anchor)
}

pub fn resolve_ref_detail(
    index_conn: Option<&Connection>,
    root: Option<&Path>,
    r: &str,
    anchor: i64,
) -> ResolvedRef {
    resolve_ref_detail_with_canon(index_conn, root, None, r, anchor)
}

/// Resolve one `--refs` token against `index_conn` and optional `root`,
/// accepting a pre-canonicalized `canon_root` for batch checks.
pub fn resolve_ref_detail_with_canon(
    index_conn: Option<&Connection>,
    root: Option<&Path>,
    canon_root: Option<&Path>,
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
        failure_kind: None,
    };
    let r_path = split_line_suffix(r);
    let Some(iconn) = index_conn else {
        if let Some((size, mtime_secs, is_dir)) =
            root.and_then(|repo| check_disk_ref_inner(repo, canon_root, r_path))
        {
            let (kind, detail) = if is_dir {
                ("path", format!("{r_path}/"))
            } else {
                ("file", format!("{r_path} ({size} bytes)"))
            };
            return from_mtime(kind, detail, mtime_secs);
        }
        return ResolvedRef {
            status: RefStatus::Unverified,
            kind: "path",
            detail: String::new(),
            mtime_secs: None,
            failure_kind: None,
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
    let file_like_eligible = r_path.contains('/') || has_known_file_ext(r_path);
    let file_hit = query_file("SELECT path, size, mtime FROM files WHERE path = ?1 LIMIT 1", r_path)
        .or_else(|| {
            file_like_eligible.then(|| {
                query_file(
                    "SELECT path, size, mtime FROM files WHERE path LIKE ?1 ESCAPE '\\' ORDER BY mtime DESC, path ASC LIMIT 1",
                    &format!("%/{}", like_escape(r_path)),
                )
            })?
        });
    if let Some((path, size, mtime_nanos)) = file_hit {
        return from_mtime(
            "file",
            format!("{path} ({size} bytes)"),
            mtime_nanos / 1_000_000_000,
        );
    }
    if let Some((fp, ls, le, mtime_nanos)) = lookup_symbol_ref(iconn, r) {
        return from_mtime(
            "symbol",
            format!("{fp}:{ls}-{le}"),
            mtime_nanos / 1_000_000_000,
        );
    }
    if let Some((size, mtime_secs, is_dir)) =
        root.and_then(|repo| check_disk_ref_inner(repo, canon_root, r_path))
    {
        let (kind, detail) = if is_dir {
            ("path", format!("{r_path}/"))
        } else {
            ("file", format!("{r_path} ({size} bytes)"))
        };
        return from_mtime(kind, detail, mtime_secs);
    }
    ResolvedRef {
        status: RefStatus::Broken,
        kind: "path",
        detail: String::new(),
        mtime_secs: None,
        failure_kind: Some(classify_broken_ref(r_path, root)),
    }
}

fn classify_broken_ref(clean: &str, root: Option<&Path>) -> RefFailureKind {
    if clean.is_empty() || clean.contains('\0') || clean.contains('\\') {
        return RefFailureKind::SymlinkOrEscape;
    }
    let p = Path::new(clean);
    if p.is_absolute() {
        return RefFailureKind::ExternalAbsPath;
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return RefFailureKind::SymlinkOrEscape;
    }
    if let Some(repo) = root {
        let mut cur = repo.to_path_buf();
        for comp in p.components() {
            cur.push(comp);
            if let Ok(meta) = std::fs::symlink_metadata(&cur) {
                if meta.file_type().is_symlink() {
                    return RefFailureKind::SymlinkOrEscape;
                }
            } else {
                break;
            }
        }
    }
    if let Some(std::path::Component::Normal(first)) = p.components().next() {
        if first == ".tmp" || first == "tmp" {
            return RefFailureKind::EphemeralTmpPath;
        }
    }
    if clean.starts_with("docs/")
        || clean.ends_with(".md")
        || clean.ends_with(".txt")
        || clean.ends_with(".rst")
    {
        return RefFailureKind::MissingDocOrReport;
    }
    if clean.contains("::") || (!clean.contains('/') && !has_known_file_ext(clean)) {
        return RefFailureKind::MissingSymbol;
    }
    RefFailureKind::MissingCodePath
}

pub(crate) fn lookup_symbol_ref(iconn: &Connection, r: &str) -> Option<(String, i64, i64, i64)> {
    let query_sym = |sql: &str, param: &str| -> Option<(String, i64, i64, i64)> {
        iconn
            .prepare_cached(sql)
            .ok()?
            .query_row(rusqlite::params![param], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .ok()
    };
    const EXACT_SQL: &str =
        "SELECT f.path, s.line_start, s.line_end, f.mtime FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.qualname = ?1 OR s.name = ?1 ORDER BY f.mtime DESC, s.qualname, s.file_path LIMIT 1";
    const SUFFIX_SQL: &str =
        "SELECT f.path, s.line_start, s.line_end, f.mtime FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.qualname LIKE ?1 ESCAPE '\\' ORDER BY f.mtime DESC, s.qualname, s.file_path LIMIT 1";

    if let Some(hit) = query_sym(EXACT_SQL, r) {
        return Some(hit);
    }
    let norm = r.replace("::", ".");
    if norm != r {
        if let Some(hit) = query_sym(EXACT_SQL, &norm) {
            return Some(hit);
        }
    }
    if norm.contains('.') {
        return query_sym(SUFFIX_SQL, &format!("%.{}", like_escape(&norm)));
    }
    None
}

pub(crate) fn check_ref_status_with_root(
    index_conn: &rusqlite::Connection,
    root: Option<&Path>,
    r: &str,
    anchor: i64,
) -> RefStatus {
    resolve_ref_detail(Some(index_conn), root, r, anchor).status
}
