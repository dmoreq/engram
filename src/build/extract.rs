//! Fresh extraction pipeline: batch absorption, healed re-extraction, and
//! the parallel chunked extract workers.

use super::*;

/// Extract one healed batch and absorb it: move the files from the carried
/// pool to fresh, subtract their old error count from the carried pool
/// (they re-extract here, so their errors arrive via `fe.error_files`
/// instead), and append the extraction output. Returns the batch's rels so
/// the caller can seed the next dirty closure.
fn absorb_batch(
    cls: &mut Classified,
    carried_errors: &mut usize,
    fe: &mut FreshExtract,
    c: &rusqlite::Connection,
    batch: Vec<crate::walk::WalkedFile>,
) -> Result<Vec<String>> {
    let healed: Vec<String> = batch.iter().map(|f| f.rel.clone()).collect();
    *carried_errors -= crate::db::old_error_count(c, &healed)?;
    let base = cls.fresh.len();
    let mut res = extract_units(&batch, base)?;
    cls.fresh.extend(batch);
    fe.rows.append(&mut res.rows);
    fe.extractions.append(&mut res.extractions);
    fe.error_files += res.error_files;
    fe.secs += res.secs;
    Ok(healed)
}

/// Re-extract the healed batch, then re-apply the inbound closure to each
/// newly fresh set until it is stable (ANEW1).
pub(super) fn extract_healed(
    prep: &mut BuildPrep,
    fe: &mut FreshExtract,
    second: Vec<crate::walk::WalkedFile>,
) -> Result<()> {
    let Some(c) = prep.old.as_ref() else {
        return Ok(());
    };
    let mut healed = absorb_batch(&mut prep.cls, &mut prep.carried_errors, fe, c, second)?;

    // ANEW1: gap healing moves cls.carried files to cls.fresh after the
    // initial dirty closure. Their inbound callers are still
    // cls.carried and point at the old symbol ids, so re-apply the
    // inbound closure to each newly cls.fresh set until it is stable.
    while !healed.is_empty() {
        let inbound = crate::db::dirty_closure(c, &healed)?;
        let third = take_fresh_batch(
            inbound,
            &mut prep.cls.carried,
            &prep.by_rel,
            &prep.stats.files,
        );
        if third.is_empty() {
            break;
        }
        healed = absorb_batch(&mut prep.cls, &mut prep.carried_errors, fe, c, third)?;
    }
    Ok(())
}
/// Parse + extract one batch of walked files on scoped workers (Amendment 11c
/// parallel parse; R6d Blocker 1: 64 MB stacks). Trees never cross threads (not
/// Send); only owned extraction rows do. Files are dispatched via `AtomicUsize`
/// in descending size order to avoid tail-latency stragglers, and outputs are
/// placed back at their original `base_idx + orig_idx` positions.
pub(super) fn extract_units(
    files: &[crate::walk::WalkedFile],
    base_idx: usize,
) -> Result<FreshExtract> {
    if files.is_empty() {
        return Ok(FreshExtract {
            rows: Vec::new(),
            extractions: Vec::new(),
            error_files: 0,
            secs: 0.0,
        });
    }
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| files[b].size.cmp(&files[a].size));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = worker_count().min(files.len());
    let mut ordered: Vec<Option<(crate::db::FilesRow, crate::extract::FileExtraction)>> =
        (0..files.len()).map(|_| None).collect();
    let mut extract_secs_sum = 0f64;
    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let next = &next;
            let order = &order;
            let handle = std::thread::Builder::new()
                .spawn_scoped(scope, move || extract_worker(files, base_idx, order, next))?;
            handles.push(handle);
        }
        for h in handles {
            let (units, secs) = h
                .join()
                .map_err(|_| EgError::Internal("parse worker panicked".to_string()))??;
            extract_secs_sum += secs;
            for (orig_idx, row, fx) in units {
                ordered[orig_idx] = Some((row, fx));
            }
        }
        Ok(())
    })?;
    let mut rows = Vec::with_capacity(files.len());
    let mut extractions = Vec::with_capacity(files.len());
    let mut error_files = 0usize;
    for slot in ordered {
        let (row, fx) = slot.ok_or_else(|| EgError::Internal("missing extracted slot".into()))?;
        if row.error_nodes > 0 {
            error_files += 1;
        }
        rows.push(row);
        extractions.push(fx);
    }
    Ok(FreshExtract {
        rows,
        extractions,
        error_files,
        secs: extract_secs_sum,
    })
}

type WorkerUnit = (usize, crate::db::FilesRow, crate::extract::FileExtraction);

fn extract_worker(
    files: &[crate::walk::WalkedFile],
    base_idx: usize,
    order: &[usize],
    next: &std::sync::atomic::AtomicUsize,
) -> Result<(Vec<WorkerUnit>, f64)> {
    let mut units = Vec::new();
    let mut secs = 0f64;
    let mut parser = crate::parse::ParserSlot::new();
    loop {
        let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if k >= order.len() {
            break;
        }
        let orig_idx = order[k];
        let (row, fx, one_secs) = extract_one(base_idx + orig_idx, &files[orig_idx], &mut parser)?;
        secs += one_secs;
        units.push((orig_idx, row, fx));
    }
    Ok((units, secs))
}

fn worker_count() -> usize {
    std::env::var("ENGRAM_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        })
        .clamp(1, 8)
}

/// Parse and extract one walked file. Returns its row, extraction, and the
/// extraction-only elapsed seconds (read and parse are excluded, as before).
fn extract_one(
    idx: usize,
    file: &crate::walk::WalkedFile,
    parser: &mut crate::parse::ParserSlot,
) -> Result<(crate::db::FilesRow, crate::extract::FileExtraction, f64)> {
    let bytes = crate::walk::read_bounded(&file.abs)?;
    // The walk allowlist guarantees a known extension; an unknown one is an
    // internal invariant break, not a silent JavaScript default.
    let lang = crate::parse::Lang::from_rel(&file.rel)
        .ok_or_else(|| EgError::Internal(format!("unsupported source extension: {}", file.rel)))?;
    let tree = parser.parse(&bytes, lang)?;
    let crate::parse::SyntaxStats {
        error_nodes,
        status,
    } = crate::parse::syntax_stats(tree.root_node());
    let e0 = Instant::now();
    let fx = crate::extract::extract_file_nodes(
        crate::extract::FileInput {
            file_idx: idx,
            rel: &file.rel,
            lang,
            src: &bytes,
        },
        tree.root_node(),
    );
    let secs = e0.elapsed().as_secs_f64();
    Ok((
        crate::db::FilesRow {
            rel: file.rel.clone(),
            size: file.size as i64,
            mtime: file.mtime,
            content_hash: crate::hash::fnv1a64(&bytes) as i64,
            parse_status: status,
            error_nodes,
            unresolved: 0,
        },
        fx,
        secs,
    ))
}
