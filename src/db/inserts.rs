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
        module_stars: load_carried_stars(old, &rel_params)?,
    })
}

/// Targeted version of `symbol_qualname_files` for incremental gap healing:
/// only queries the qualnames touched by fresh or changed files.
pub fn symbol_qualname_files_for(
    conn: &Connection,
    qualnames: &[String],
) -> Result<HashMap<String, HashSet<String>>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    if qualnames.is_empty() {
        return Ok(out);
    }
    for_each_chunk(conn, qualnames, |conn, placeholders, chunk| {
        let mut stmt = conn.prepare(&format!(
            "SELECT qualname, file_path FROM symbols WHERE qualname IN ({placeholders})"
        ))?;
        let mut rows = stmt.query(rusqlite::params_from_iter(chunk.iter()))?;
        while let Some(r) = rows.next()? {
            let qualname: String = r.get(0)?;
            let path: String = r.get(1)?;
            out.entry(qualname).or_default().insert(path);
        }
        Ok(())
    })?;
    Ok(out)
}

fn load_carried_symbols(old: &Connection, rel_params: &[&str]) -> Result<Vec<CarriedSymbol>> {
    let mut symbols = Vec::new();
    let mut mod_cache: HashMap<String, String> = HashMap::new();
    for_each_chunk(old, rel_params, |old, placeholders, chunk| {
        let mut stmt = old.prepare(&format!(
            "SELECT s.id, s.kind, s.name, s.qualname, s.file_path, s.return_type
             FROM symbols s
             WHERE s.file_path IN ({placeholders})"
        ))?;
        let mut part = stmt.query(rusqlite::params_from_iter(chunk.iter().copied()))?;
        while let Some(r) = part.next()? {
            let kind_str: String = r.get(1)?;
            let Some(kind) = SymbolKind::parse(&kind_str) else {
                continue;
            };
            let path: String = r.get(4)?;
            let module = mod_cache
                .entry(path)
                .or_insert_with_key(|p| crate::extract::module_path_for(p).0)
                .clone();
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
            "SELECT me.module_qualname, me.exported_name, me.target_sym_id
             FROM module_exports me
             JOIN files f ON f.id = me.file_id
             WHERE f.path IN ({placeholders})
             ORDER BY me.rowid"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            reexports.push(row?);
        }
        Ok(())
    })?;
    Ok(reexports)
}

fn load_carried_stars(old: &Connection, rel_params: &[&str]) -> Result<Vec<(String, String)>> {
    let mut stars = Vec::new();
    for_each_chunk(old, rel_params, |old, placeholders, chunk| {
        let mut stmt = old.prepare(&format!(
            "SELECT ms.module_qualname, ms.star_module
             FROM module_stars ms
             JOIN files f ON f.id = ms.file_id
             WHERE f.path IN ({placeholders})
             ORDER BY ms.rowid"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            stars.push(row?);
        }
        Ok(())
    })?;
    Ok(stars)
}

fn load_carried_inherits(old: &Connection, rel_params: &[&str]) -> Result<Vec<(String, String)>> {
    // e.id is the insertion-order anchor: sorting on it makes the carried
    // MRO edge order identical between fresh and incremental builds, so
    // multi-inheritance dispatch stays deterministic.
    let mut inherits: Vec<(i64, String, String)> = Vec::new();
    for_each_chunk(old, rel_params, |old, placeholders, chunk| {
        let mut stmt = old.prepare(&format!(
            "SELECT e.id, s.qualname, e.dst_name
             FROM edges e
             JOIN symbols s ON s.id = e.src_id
             JOIN symbols ds ON ds.id = e.dst_id
             JOIN files f ON f.id = s.file_id
             WHERE e.kind = '{}'
               AND e.dst_id IS NOT NULL
               AND e.confidence = '{}'
               AND NOT (s.kind = '{}' AND ds.kind IN ('{}', '{}'))
               AND f.path IN ({placeholders})
             ORDER BY e.id",
            EdgeKind::Inherits.as_str(),
            Confidence::Extracted.as_str(),
            SymbolKind::Class.as_str(),
            SymbolKind::Trait.as_str(),
            SymbolKind::Interface.as_str(),
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
         WHERE file_id IN (SELECT id FROM files);
         INSERT INTO module_exports(file_id, module_qualname, exported_name, target_sym_id)
         SELECT file_id, module_qualname, exported_name, target_sym_id
         FROM olddb.module_exports
         WHERE file_id IN (SELECT id FROM files)
           AND target_sym_id IN (SELECT id FROM symbols);
         INSERT INTO module_stars(file_id, module_qualname, star_module)
         SELECT file_id, module_qualname, star_module
         FROM olddb.module_stars WHERE file_id IN (SELECT id FROM files);
         INSERT INTO symbols_fts(rowid, text)
         SELECT fts.rowid, fts.text FROM olddb.symbols_fts fts
         WHERE fts.rowid IN (SELECT id FROM symbols);",
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

/// Insert resolved module exports for fresh files (schema v12).
pub fn insert_module_exports(
    conn: &Connection,
    exports: &[(usize, &str, &str, i64)],
    file_ids: &[i64],
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO module_exports(file_id, module_qualname, exported_name, target_sym_id)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for &(file_idx, mod_qn, exported, target_id) in exports {
        stmt.execute(rusqlite::params![
            file_ids[file_idx],
            mod_qn,
            exported,
            target_id
        ])?;
    }
    Ok(())
}

/// Insert resolved module star re-exports for fresh files (schema v12).
pub fn insert_module_stars(
    conn: &Connection,
    stars: &[crate::extract::ResolvedModuleStar],
    file_ids: &[i64],
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO module_stars(file_id, module_qualname, star_module)
         VALUES (?1, ?2, ?3)",
    )?;
    for s in stars {
        stmt.execute(rusqlite::params![
            file_ids[s.file_idx],
            s.module_qualname.as_str(),
            s.star_module.as_str()
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
fn write_fts_text(buf: &mut String, qualname: &str, name: &str, path: &str, signature: &str) {
    use std::fmt::Write;
    buf.clear();
    let q = crate::extract::split_ident(qualname);
    let n = crate::extract::split_ident(name);
    let p = crate::extract::split_ident(path);
    let s = crate::extract::split_ident(signature);
    let _ = write!(buf, "{q} {q} {n} {n} {p} {s}");
}

pub fn populate_symbols_fts(conn: &Connection) -> Result<()> {
    // Stream fresh symbol rows straight into the FTS insert (carried rows
    // were already copied from olddb.symbols_fts in carry_forward_unchanged).
    let max_carried_id: i64 =
        conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM symbols_fts", [], |r| {
            r.get(0)
        })?;
    let mut ins = conn.prepare("INSERT INTO symbols_fts(rowid, text) VALUES (?1, ?2)")?;
    let mut stmt = conn.prepare(
        "SELECT id, qualname, name, file_path, signature
         FROM symbols
         WHERE id > ?1
         ORDER BY id",
    )?;
    let mut buf = String::new();
    let mut rows = stmt.query([max_carried_id])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let qualname = r.get_ref(1)?.as_str().map_err(rusqlite::Error::from)?;
        let name = r.get_ref(2)?.as_str().map_err(rusqlite::Error::from)?;
        let path = r.get_ref(3)?.as_str().map_err(rusqlite::Error::from)?;
        let signature = r.get_ref(4)?.as_str().map_err(rusqlite::Error::from)?;
        write_fts_text(&mut buf, qualname, name, path, signature);
        ins.execute(rusqlite::params![id, buf.as_str()])?;
    }
    Ok(())
}
