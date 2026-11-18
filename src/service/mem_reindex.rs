//! `eg mem reindex`: force a full rebuild of the derived Unicode memory index.
//! Notes are primary data and are never touched; only the v2 FTS table and the
//! per-row `fts_version` markers are rewritten. `--embed` additionally
//! computes note embeddings through the eg-judge text model (NULL when the
//! model is not configured).

use super::*;

pub(super) fn run_mem_reindex(req: &CommandRequest, root_arg: Option<&Path>) -> Result<ArmOut> {
    let CommandRequest::MemReindex { global, embed } = req else {
        unreachable!("run_mem_reindex called with a non-reindex request")
    };
    let conn = open_mem_conn(root_arg, *global, MemoryMode::Mutation)?;
    let rows = crate::db::ensure_memory_fts_v2(&conn, true)?;
    let mut stderr = String::new();
    let mut embedded = 0usize;
    if *embed {
        if crate::memory::embed::text_model_dir().is_none() {
            stderr.push_str(
                "eg: reindex: no text model configured (set EG_JUDGE_TEXT_MODEL); embeddings left NULL\n",
            );
        } else {
            for (id, body) in crate::db::memory_current_note_bodies(&conn)? {
                match crate::memory::embed::embed_text(&body) {
                    Some(vector) => {
                        conn.execute(
                            "UPDATE memories SET embedding = ?1 WHERE id = ?2",
                            rusqlite::params![crate::memory::embed::embedding_blob(&vector), id],
                        )
                        .map_err(crate::db::map_memory_err)?;
                        embedded += 1;
                    }
                    None => {
                        stderr.push_str(&format!(
                            "eg: reindex: embedding failed for row {id}; left NULL\n"
                        ));
                    }
                }
            }
        }
    }
    crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
    let mut line = format!("reindexed {rows} rows\n");
    if *embed {
        line.push_str(&format!("embedded {embedded} notes\n"));
    }
    Ok(ArmOut::plain(line, stderr))
}
