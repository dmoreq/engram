//! Build-phase inserts: files, symbols, edges, FTS population, and
//! carry-forward of unchanged rows under the caller-owned transaction.

use super::*;

/// Insert unresolved edge sites for the fresh rows (schema v6, Amendment 19).
pub fn insert_unresolved_calls(
    conn: &Connection,
    unresolved: &[crate::extract::UnresolvedSite],
    file_ids: &[i64],
) -> Result<()> {
    if unresolved.iter().any(|u| u.site_line == 0) {
        return Err(crate::error::EgError::Internal(
            "unresolved edge row has site_line 0".to_string(),
        ));
    }
    let mut stmt = conn.prepare(
        "INSERT INTO unresolved_calls(file_id, dst_name, kind, site_line) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for u in unresolved {
        stmt.execute(rusqlite::params![
            file_ids[u.file_idx],
            u.name.as_str(),
            u.kind.as_str(),
            u.site_line as i64
        ])?;
    }
    Ok(())
}

/// Symbols + inherits + class_fields + reexports of the carried set, for resolution tables.
pub fn load_carried(old: &Connection, rels: &HashSet<String>) -> Result<CarriedIndex> {
    if rels.is_empty() {
        return Ok(CarriedIndex::default());
    }
    let mut rel_params: Vec<&str> = rels.iter().map(String::as_str).collect();
    rel_params.sort_unstable();
    Ok(CarriedIndex {
        symbols: load_carried_symbols(old, &rel_params)?,
        inherits: load_carried_inherits(old, &rel_params)?,
        class_fields: load_carried_class_fields(old, &rel_params)?,
        reexports: load_carried_reexports(old, &rel_params)?,
    })
}

fn load_carried_symbols(old: &Connection, rel_params: &[&str]) -> Result<Vec<CarriedSymbol>> {
    let mut symbols = Vec::new();
    for_each_chunk(old, rel_params, |old, placeholders, chunk| {
        let mut stmt = old.prepare(&format!(
            "SELECT s.id, s.kind, s.name, s.qualname, f.path, s.return_type
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path IN ({placeholders})"
        ))?;
        let mut part = stmt.query(rusqlite::params_from_iter(chunk.iter().copied()))?;
        while let Some(r) = part.next()? {
            let kind_str: String = r.get(1)?;
            let Some(kind) = SymbolKind::parse(&kind_str) else {
                continue;
            };
            let path: String = r.get(4)?;
            let (module, _) = crate::extract::module_path_for(&path);
            symbols.push(CarriedSymbol {
                row_id: r.get(0)?,
                kind,
                name: r.get(2)?,
                qualname: r.get(3)?,
                module,
                return_type: r.get(5)?,
            });
        }
        Ok(())
    })?;
    symbols.sort_by_key(|s| s.row_id);
    Ok(symbols)
}

fn load_carried_class_fields(
    old: &Connection,
    rel_params: &[&str],
) -> Result<Vec<(String, String, String)>> {
    let mut fields = Vec::new();
    for_each_chunk(old, rel_params, |old, placeholders, chunk| {
        let mut stmt = old.prepare(&format!(
            "SELECT cf.class_qualname, cf.field, cf.target_qualname
             FROM class_fields cf JOIN files f ON f.id = cf.file_id
             WHERE f.path IN ({placeholders})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            fields.push(row?);
        }
        Ok(())
    })?;
    Ok(fields)
}

fn load_carried_reexports(
    old: &Connection,
    rel_params: &[&str],
) -> Result<Vec<(String, String, i64)>> {
    let mut reexports = Vec::new();
    for_each_chunk(old, rel_params, |old, placeholders, chunk| {
        let mut stmt = old.prepare(&format!(
            "SELECT ss.qualname, e.dst_name, e.dst_id
             FROM edges e
             JOIN symbols ss ON ss.id = e.src_id
             JOIN files f ON f.id = ss.file_id
             WHERE e.kind = '{}' AND ss.kind = '{}' AND e.dst_id IS NOT NULL
               AND f.path IN ({placeholders})",
            EdgeKind::Imports.as_str(),
            SymbolKind::Module.as_str()
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (mod_qn, dst_name, dst_id) = row?;
            if let Some(exported) = dst_name.rsplit('.').next() {
                reexports.push((mod_qn, exported.to_string(), dst_id));
            }
        }
        Ok(())
    })?;
    Ok(reexports)
}

fn load_carried_inherits(old: &Connection, rel_params: &[&str]) -> Result<Vec<(String, String)>> {
    // e.id is the insertion-order anchor: sorting on it makes the carried
    // MRO edge order identical between fresh and incremental builds, so
    // multi-inheritance dispatch stays deterministic.
    let mut inherits: Vec<(i64, String, String)> = Vec::new();
    for_each_chunk(old, rel_params, |old, placeholders, chunk| {
        let mut stmt = old.prepare(&format!(
            "SELECT e.id, s.qualname, e.dst_name
             FROM edges e JOIN symbols s ON s.id = e.src_id
             JOIN files f ON f.id = s.file_id
             WHERE e.kind = '{}' AND e.dst_name IS NOT NULL
               AND f.path IN ({placeholders})
             ORDER BY e.id",
            EdgeKind::Inherits.as_str()
        ))?;
        let pairs = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for p in pairs {
            inherits.push(p?);
        }
        Ok(())
    })?;
    inherits.sort_by_key(|row| row.0);
    Ok(inherits
        .into_iter()
        .map(|(_, qualname, dst)| (qualname, dst))
        .collect())
}

/// One hash-carry row for `carry_forward_unchanged`: the SELECT locates the
/// old row by (rel, old_mtime, size) and refreshes its stored mtime to
/// `current_mtime` when the fresh checkout differs (Amendment 9).
pub struct CarryRow {
    pub rel: String,
    pub old_mtime: i64,
    pub size: i64,
    pub current_mtime: i64,
}

/// Copy rows for files matching the fast-path keys: the SELECT identifies the
/// old row by (path, old_mtime, size); hash-carried rows (Amendment 9) then
/// get their stored mtime refreshed to the fresh checkout's stat so the
/// staleness contract stays truthful. Attaches the old DB itself (SQLite
/// ATTACH takes no bound parameters).
/// Amendment 11b: the caller owns the build transaction — no BEGIN/COMMIT here.
pub fn carry_forward_unchanged(
    new: &Connection,
    root: &Path,
    unchanged: &[CarryRow],
) -> Result<()> {
    let old_path = db_path(root);
    let quoted = old_path.to_string_lossy().replace('\'', "''");
    new.execute_batch(&format!("ATTACH DATABASE '{quoted}' AS olddb"))?;
    {
        let mut stmt = new.prepare(
            "INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes, unresolved)
             SELECT id, path, size, ?4, content_hash, parse_status, error_nodes, unresolved
             FROM olddb.files WHERE path = ?1 AND mtime = ?2 AND size = ?3",
        )?;
        for row in unchanged {
            stmt.execute(rusqlite::params![
                row.rel,
                row.old_mtime,
                row.size,
                row.current_mtime
            ])?;
        }
    }
    new.execute_batch(
        "INSERT INTO symbols(id, kind, name, qualname, file_id, file_path, line_start, line_end, signature, is_test, return_type)
         SELECT id, kind, name, qualname, file_id, file_path, line_start, line_end, signature, is_test, return_type
         FROM olddb.symbols WHERE file_id IN (SELECT id FROM files);
         INSERT INTO class_fields(file_id, class_qualname, field, target_qualname)
         SELECT file_id, class_qualname, field, target_qualname
         FROM olddb.class_fields WHERE file_id IN (SELECT id FROM files);
         INSERT INTO edges(id, src_id, dst_id, dst_name, kind, confidence, site_line)
         SELECT id, src_id, dst_id, dst_name, kind, confidence, site_line
         FROM olddb.edges
         WHERE src_id IN (SELECT id FROM symbols)
           AND (dst_id IS NULL OR dst_id IN (SELECT id FROM symbols));
         INSERT INTO unresolved_calls(file_id, dst_name, kind, site_line)
         SELECT file_id, dst_name, kind, site_line FROM olddb.unresolved_calls
         WHERE file_id IN (SELECT id FROM files);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FilesRow {
    pub rel: String,
    pub size: i64,
    pub mtime: i64,
    pub content_hash: i64,
    pub parse_status: &'static str,
    pub error_nodes: usize,
    /// Per-file unresolved-callee count (filled after resolve()).
    pub unresolved: usize,
}

/// Amendment 11b: caller holds the build transaction; 500-row prepared
/// batches remain (statement reuse), the per-chunk BEGIN/COMMIT fsyncs do not.
pub fn insert_files(conn: &Connection, rows: &[FilesRow]) -> Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(rows.len());
    let mut stmt = conn.prepare(
        "INSERT INTO files(path, size, mtime, content_hash, parse_status, error_nodes, unresolved)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for r in rows {
        stmt.execute(rusqlite::params![
            r.rel,
            r.size,
            r.mtime,
            r.content_hash,
            r.parse_status,
            r.error_nodes as i64,
            r.unresolved as i64
        ])?;
        ids.push(conn.last_insert_rowid());
    }
    Ok(ids)
}

pub fn insert_symbols(
    conn: &Connection,
    symbols: &[&NewSymbol],
    file_ids: &[i64],
    file_paths: &[String],
) -> Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(symbols.len());
    let mut stmt = conn.prepare(
        "INSERT INTO symbols(kind, name, qualname, file_id, file_path, line_start, line_end, signature, is_test, return_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for s in symbols {
        stmt.execute(rusqlite::params![
            s.kind.as_str(),
            s.name,
            s.qualname,
            file_ids[s.file_idx],
            file_paths[s.file_idx].as_str(),
            s.line_start as i64,
            s.line_end as i64,
            s.signature,
            i64::from(s.is_test),
            s.return_type
        ])?;
        ids.push(conn.last_insert_rowid());
    }
    Ok(ids)
}

/// Insert resolved class field types for fresh files (schema v11).
pub fn insert_class_fields(
    conn: &Connection,
    fields: &[crate::extract::ResolvedClassField],
    file_ids: &[i64],
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO class_fields(file_id, class_qualname, field, target_qualname)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for f in fields {
        stmt.execute(rusqlite::params![
            file_ids[f.file_idx],
            f.class_qualname.as_str(),
            f.field.as_str(),
            f.target_qualname.as_str()
        ])?;
    }
    Ok(())
}

/// Fully resolved edge row: endpoint ids already mapped (fresh -> inserted id,
/// carried -> previous row id).
#[derive(Debug)]
pub struct EdgeRow<'a> {
    pub src_id: i64,
    pub dst_id: Option<i64>,
    pub dst_name: &'a str,
    pub kind: &'static str,
    pub confidence: Confidence,
    pub site_line: usize,
}

pub fn insert_edge_rows(conn: &Connection, rows: &[EdgeRow<'_>]) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO edges(src_id, dst_id, dst_name, kind, confidence, site_line)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for e in rows {
        stmt.execute(rusqlite::params![
            e.src_id,
            e.dst_id,
            e.dst_name,
            e.kind,
            e.confidence.as_str(),
            e.site_line as i64
        ])?;
    }
    Ok(())
}

/// Populate the FTS5 symbol-search index from the fully-inserted symbols
/// table (Amendment 12b): one pass covering fresh AND carried rows, called
/// inside the build transaction after `insert_edge_rows`. Amendment 14
/// widens the indexed text with path and signature tokens — the two
/// haystacks grep already matches and a name-only index could not.
/// FTS text for one symbol (Amendment 12b/14/R7): qualname and name repeat so
/// bm25 ranks identity matches above signature-only matches now that signatures
/// carry full parameter lists. Shared by the SELECT and in-memory paths so the
/// indexed bytes cannot drift.
fn fts_text(qualname: &str, name: &str, path: &str, signature: &str) -> String {
    format!(
        "{q} {q} {n} {n} {p} {s}",
        q = crate::extract::split_ident(qualname),
        n = crate::extract::split_ident(name),
        p = crate::extract::split_ident(path),
        s = crate::extract::split_ident(signature)
    )
}

pub fn populate_symbols_fts(conn: &Connection) -> Result<()> {
    // Stream symbol rows straight into the FTS insert: no bulk Vec keeps the
    // whole symbol corpus (text included) on the heap during a build.
    let mut ins = conn.prepare("INSERT INTO symbols_fts(rowid, text) VALUES (?1, ?2)")?;
    let mut stmt = conn.prepare(
        "SELECT s.id, s.qualname, s.name, f.path, s.signature
         FROM symbols s JOIN files f ON f.id = s.file_id
         ORDER BY s.id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let qualname = r.get_ref(1)?.as_str().map_err(rusqlite::Error::from)?;
        let name = r.get_ref(2)?.as_str().map_err(rusqlite::Error::from)?;
        let path = r.get_ref(3)?.as_str().map_err(rusqlite::Error::from)?;
        let signature = r.get_ref(4)?.as_str().map_err(rusqlite::Error::from)?;
        ins.execute(rusqlite::params![
            id,
            fts_text(qualname, name, path, signature)
        ])?;
    }
    Ok(())
}
