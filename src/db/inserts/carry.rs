use super::*;

/// One hash-carry row for `carry_forward_unchanged`: the SELECT locates the
/// old row by `(rel, old_mtime, size)` and refreshes its stored `mtime` to
/// `current_mtime` (Amendment 9).
#[derive(Debug, Clone)]
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
    if unchanged.is_empty() {
        return Ok(());
    }
    let old_path = db_path(root);
    let quoted = old_path.to_string_lossy().replace('\'', "''");
    new.execute_batch(&format!("ATTACH DATABASE '{quoted}' AS olddb"))?;
    for chunk in unchanged.chunks(200) {
        let values_sql: String = (0..chunk.len())
            .map(|i| {
                let b = i * 4 + 1;
                format!("(?{b},?{},?{},?{})", b + 1, b + 2, b + 3)
            })
            .collect::<Vec<_>>()
            .join(",");
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(chunk.len() * 4);
        for row in chunk {
            params.extend([
                row.rel.clone().into(),
                row.old_mtime.into(),
                row.size.into(),
                row.current_mtime.into(),
            ]);
        }
        let sql = format!(
            "WITH carried(rel, old_mtime, sz, cur_mtime) AS (VALUES {values_sql})
             INSERT INTO files(id, path, size, mtime, content_hash, parse_status, error_nodes, unresolved)
             SELECT f.id, f.path, f.size, c.cur_mtime, f.content_hash, f.parse_status, f.error_nodes, f.unresolved
             FROM olddb.files f
             JOIN carried c ON f.path = c.rel AND f.mtime = c.old_mtime AND f.size = c.sz"
        );
        new.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
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
