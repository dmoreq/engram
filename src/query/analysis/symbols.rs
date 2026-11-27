//! `eg symbols`: single-file and directory symbol listings.

use super::*;

/// Display knobs for `eg symbols` (mirrors `ImpactOpts`): `--outline` adds the
/// stored one-line signature.
pub struct SymbolsOpts {
    pub outline: bool,
}

/// Symbols of one indexed file (Amendment 5): `path:line kind qualname` by
/// line; single-file staleness header. With `outline` (Amendment 12a) each
/// line gains the stored one-line signature; without it, byte-identical to
/// the pre-v4 contract.
pub fn symbols(
    ctx: &QueryCtx<'_>,
    file: &str,
    limit: usize,
    opts: SymbolsOpts,
) -> Result<LineQuery> {
    let QueryCtx { conn, .. } = *ctx;
    let SymbolsOpts { outline } = opts;
    let limit = clamp_limit(limit)?;
    let rel = normalize_repo_path(ctx.root, file);
    // `.` and `./` mean the repository root; the empty prefix matches every
    // indexed file without generating a "/" prefix.
    let rel = if rel == "." { String::new() } else { rel };
    let Some(rel) = indexed_file_opt(conn, &rel)? else {
        let is_source_file_ext = rel
            .rsplit_once('.')
            .is_some_and(|(_, ext)| crate::parse::is_source_ext(ext));
        if is_source_file_ext && !dir_has_files(conn, &rel)? {
            return Err(EgError::not_indexed(file));
        }
        return symbols_directory(ctx, &rel, limit, SymbolsOpts { outline });
    };
    symbols_file(ctx, &rel, limit, outline)
}

fn dir_range_bounds(dir: &str) -> (String, String) {
    (format!("{dir}/"), format!("{dir}0"))
}

fn dir_has_files(conn: &Connection, dir: &str) -> Result<bool> {
    if dir.is_empty() {
        return Ok(true);
    }
    let (lower, upper) = dir_range_bounds(dir);
    let hit: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM files WHERE path >= ?1 AND path < ?2 LIMIT 1",
            [&lower, &upper],
            |r| r.get(0),
        )
        .ok();
    Ok(hit.is_some())
}

/// The single-file branch of [`symbols`]: symbol rows with the optional
/// outline signature, the staleness header, and the cap footer. `file` is the
/// original query text, kept for the not-indexed error; `rel` is the indexed
/// path `indexed_file_opt` resolved.
fn symbols_file(ctx: &QueryCtx<'_>, rel: &str, limit: usize, outline: bool) -> Result<LineQuery> {
    let QueryCtx { conn, .. } = *ctx;
    let rows = file_symbol_rows(conn, rel, outline)?;
    if rows.is_empty() {
        // The file IS indexed: "not indexed" would mislead a scope check.
        return Err(EgError::NoResults(format!(
            "0 symbols found in '{rel}' (indexed)"
        )));
    }
    let files = BTreeSet::from([rel.to_string()]);
    let (header, stale) = staleness_header(ctx, &files);
    let n_symbols = rows.len();
    let shown = n_symbols.min(limit);
    let data_lines: Vec<String> = rows
        .into_iter()
        .take(shown)
        .map(|row| symbol_line(rel, &row))
        .collect();
    let mut out = Vec::new();
    if let Some(header) = header {
        out.push(header);
    }
    out.extend(data_lines);
    let cap = finish_capped(&mut out, shown, n_symbols, stale, "");
    Ok(LineQuery::new(out, cap, shown))
}

fn file_symbol_rows(conn: &Connection, rel: &str, outline: bool) -> Result<Vec<SymbolRow>> {
    let signature_col = if outline { "s.signature" } else { "''" };
    let sql = format!(
        "SELECT s.kind, s.qualname, s.line_start, {signature_col}
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.path = ?1
         ORDER BY s.line_start, s.qualname"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([rel], |r| {
            Ok(SymbolRow {
                kind: r.get(0)?,
                qualname: r.get(1)?,
                line: r.get(2)?,
                signature: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One `path:line kind qualname` row; `--outline` appends the signature.
fn symbol_line(path: &str, row: &SymbolRow) -> String {
    let mut text = format!(
        "{}:{} {} {}",
        sanitize(path),
        row.line,
        sanitize(&row.kind),
        sanitize(&row.qualname)
    );
    if !row.signature.is_empty() {
        text.push(' ');
        text.push_str(&sanitize(&row.signature));
    }
    text
}

/// Group symbol rows under their file path and finish with the cap/journal
/// tail — the per-file rendering shared by the `symbols` directory arm.
fn dir_line_query(
    mut out: Vec<String>,
    rows: &[(String, SymbolRow)],
    total: usize,
    stale: bool,
) -> LineQuery {
    let mut last_path = "";
    for (path, row) in rows {
        if path != last_path {
            out.push(sanitize(path));
            last_path = path;
        }
        out.push(symbol_line(path, row));
    }
    let shown = rows.len();
    let cap = finish_capped(&mut out, shown, total, stale, "");
    LineQuery::new(out, cap, shown)
}

/// Directory mode for `eg symbols <dir>` (Amendment 19): indexed prefix only,
/// grouped per file, cap counts symbol data lines.
fn symbols_directory(
    ctx: &QueryCtx<'_>,
    dir: &str,
    limit: usize,
    SymbolsOpts { outline }: SymbolsOpts,
) -> Result<LineQuery> {
    let QueryCtx { conn, .. } = *ctx;
    validate_path_scope(dir, true)?;
    let display = if dir.is_empty() { "." } else { dir };
    let range = (!dir.is_empty()).then(|| dir_range_bounds(dir));
    let total: i64 = match &range {
        Some((lower, upper)) => conn.query_row(
            "SELECT COUNT(*) FROM symbols s WHERE s.file_path >= ?1 AND s.file_path < ?2",
            [lower, upper],
            |r| r.get(0),
        )?,
        None => conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?,
    };
    if total == 0 {
        let mut msg = format!("0 symbols in '{display}' (indexed)");
        if !dir.is_empty() && !dir.contains('/') && !dir.contains('.') {
            msg.push_str(
                " (hint: 'eg symbols' takes a file or directory path; for a symbol use 'eg context <name>' or 'eg search \"<terms>\"')",
            );
        }
        return Err(EgError::NoResults(msg));
    }
    let n_files: i64 = match &range {
        Some((lower, upper)) => conn.query_row(
            "SELECT COUNT(*) FROM files f WHERE f.path >= ?1 AND f.path < ?2",
            [lower, upper],
            |r| r.get(0),
        )?,
        None => conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?,
    };
    let rows = if limit == 0 {
        Vec::new()
    } else {
        dir_symbol_rows(conn, range.as_ref(), outline, limit)?
    };
    let displayed_paths: BTreeSet<String> = rows.iter().map(|(p, _)| p.clone()).collect();
    let (header, stale) = staleness_header(ctx, &displayed_paths);
    let mut out = Vec::new();
    if let Some(header) = header {
        out.push(header);
    }
    out.push(format!(
        "eg: dir {} — {n_files} files, {total} symbols",
        sanitize(display)
    ));
    Ok(dir_line_query(out, &rows, total as usize, stale))
}

fn dir_symbol_rows(
    conn: &Connection,
    range: Option<&(String, String)>,
    outline: bool,
    limit: usize,
) -> Result<Vec<(String, SymbolRow)>> {
    let sig_col = if outline { "s.signature" } else { "''" };
    let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, SymbolRow)> {
        Ok((
            r.get::<_, String>(0)?,
            SymbolRow {
                kind: r.get(1)?,
                qualname: r.get(2)?,
                line: r.get(3)?,
                signature: r.get(4)?,
            },
        ))
    };
    let rows = match range {
        Some((lower, upper)) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT s.file_path, s.kind, s.qualname, s.line_start, {sig_col}
                 FROM symbols s
                 WHERE s.file_path >= ?1 AND s.file_path < ?2
                 ORDER BY s.file_path, s.line_start, s.qualname LIMIT ?3"
            ))?;
            let out = stmt
                .query_map(rusqlite::params![lower, upper, limit as i64], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out
        }
        None => {
            let mut stmt = conn.prepare(&format!(
                "SELECT s.file_path, s.kind, s.qualname, s.line_start, {sig_col}
                 FROM symbols s
                 ORDER BY s.file_path, s.line_start, s.qualname LIMIT ?1"
            ))?;
            let out = stmt
                .query_map(rusqlite::params![limit as i64], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out
        }
    };
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_directory_zero_emits_hint_for_bare_identifier() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL);
             CREATE TABLE symbols (id INTEGER PRIMARY KEY, file_id INTEGER, file_path TEXT, kind TEXT, qualname TEXT, line_start INTEGER, signature TEXT);",
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: std::path::Path::new("."),
        };
        let err = symbols(&ctx, "my_func", 50, SymbolsOpts { outline: false })
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("'eg symbols' takes a file or directory path; for a symbol use 'eg context <name>' or 'eg search \"<terms>\"'"),
            "{err}"
        );
        let dir_err = symbols(&ctx, "src/missing", 50, SymbolsOpts { outline: false })
            .err()
            .unwrap()
            .to_string();
        assert!(!dir_err.contains("eg context"), "{dir_err}");
    }

    #[test]
    fn symbols_accepts_extension_named_directories() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT NOT NULL, size INTEGER DEFAULT 0, mtime INTEGER DEFAULT 0, content_hash INTEGER DEFAULT 0);
             CREATE TABLE symbols (id INTEGER PRIMARY KEY, file_id INTEGER, file_path TEXT, kind TEXT, qualname TEXT, line_start INTEGER, signature TEXT);
             INSERT INTO files (id, path) VALUES (1, 'ts/app.ts'), (2, 'py/mod.py');
             INSERT INTO symbols VALUES (1, 1, 'ts/app.ts', 'function', 'ts.app.run', 1, '');
             INSERT INTO symbols VALUES (2, 2, 'py/mod.py', 'function', 'py.mod.main', 1, '');",
        )
        .unwrap();
        let ctx = QueryCtx {
            conn: &conn,
            root: std::path::Path::new("."),
        };
        let ts_out = symbols(&ctx, "ts/", 50, SymbolsOpts { outline: false }).unwrap();
        assert!(ts_out.lines.iter().any(|l| l.contains("ts.app.run")));
        let py_out = symbols(&ctx, "py", 50, SymbolsOpts { outline: false }).unwrap();
        assert!(py_out.lines.iter().any(|l| l.contains("py.mod.main")));
    }
}
