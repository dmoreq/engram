//! Command dispatch: routes each `CommandRequest` family to its executor
//! and adapts query/graph/traversal results into rendered `ArmOut` text.

use super::*;

pub(super) fn run(
    req: &CommandRequest,
    root_arg: Option<&Path>,
    surface: Surface,
) -> Result<ArmOut> {
    match req {
        CommandRequest::Build {
            force,
            max_parse_error_rate,
        } => {
            crate::db::evict_mcp_cache(root_arg);
            let (line, stderr, shown) =
                crate::build::run_build(root_arg, *force, *max_parse_error_rate)?;
            Ok(ArmOut {
                stdout: format!("{line}\n"),
                stderr,
                shown: Some(shown),
                exit_code: None,
            })
        }
        CommandRequest::Doctor { invalidate } => run_doctor(root_arg, *invalidate),
        CommandRequest::Usage { quality, global } => run_usage(root_arg, *quality, *global),
        CommandRequest::Audit {
            json,
            global,
            days,
            baseline_days,
            all_checks,
            reveal_paths,
            fail_on,
        } => {
            let out = crate::audit::execute(
                &crate::audit::AuditRequest {
                    json: *json,
                    global: *global,
                    days: *days,
                    baseline_days: *baseline_days,
                    all_checks: *all_checks,
                    reveal_paths: *reveal_paths,
                    fail_on: fail_on.clone(),
                },
                root_arg,
            )?;
            Ok(ArmOut {
                stdout: out.stdout,
                stderr: String::new(),
                shown: None,
                exit_code: Some(out.exit_code),
            })
        }
        CommandRequest::MemWrite { .. } => run_mem_write(req, root_arg, surface),
        CommandRequest::MemTouch { .. } => run_mem_touch(req, root_arg),
        CommandRequest::MemQuery { .. } => run_mem_query(req, root_arg, surface),
        CommandRequest::MemGet { .. } => run_mem_get(req, root_arg, surface),
        CommandRequest::MemDelete { .. } => run_mem_delete(req, root_arg),
        CommandRequest::MemGc { .. } => run_mem_gc(req, root_arg),
        CommandRequest::MemExport { .. } => run_mem_export(req, root_arg, surface),
        CommandRequest::MemImport { .. } => run_mem_import(req, root_arg, surface),
        CommandRequest::MemReindex { .. } => run_mem_reindex(req, root_arg),
        _ => run_query(req, root_arg, surface),
    }
}

pub(super) fn run_doctor(root_arg: Option<&Path>, invalidate: bool) -> Result<ArmOut> {
    let root = crate::walk::discover_root(root_arg)?;
    let lines = if invalidate {
        crate::doctor::invalidate(&root)?
    } else {
        crate::doctor::report(&root)?
    };
    Ok(ArmOut::plain(join_lines(lines), String::new()))
}

pub(super) fn run_usage(root_arg: Option<&Path>, quality: bool, global: bool) -> Result<ArmOut> {
    if global {
        let lines = crate::journal::global_summary(quality)?;
        return Ok(ArmOut::plain(join_lines(lines), String::new()));
    }
    let root = match crate::walk::discover_root(root_arg) {
        Ok(r) => r,
        Err(e) => {
            return Err(EgError::NoResults(format!(
                "{e}\n(hint: to view machine-wide telemetry across all repos, run: eg usage --global)"
            )));
        }
    };
    match crate::journal::summary(&root, quality) {
        Ok(lines) => Ok(ArmOut::plain(join_lines(lines), String::new())),
        Err(EgError::NoResults(msg)) => Err(EgError::NoResults(format!(
            "{msg}\n(hint: to view machine-wide telemetry across all repos, run: eg usage --global)"
        ))),
        Err(e) => Err(e),
    }
}

fn validate_reuse_args(req: &CommandRequest) -> Result<()> {
    let CommandRequest::Reuse {
        paths,
        limit,
        min_confidence,
        propose,
        callees,
        coverage,
        ..
    } = req
    else {
        unreachable!()
    };
    // 0 is the count-only convention: summary totals, empty payload.
    if *limit > 100 {
        return Err(EgError::Usage("limit must be 0..=100".to_string()));
    }
    if !min_confidence.is_empty()
        && crate::query::reuse::ReuseConfidence::parse(min_confidence).is_none()
    {
        return Err(EgError::Usage(
            "min-confidence must be low|provisional|medium|high".to_string(),
        ));
    }
    match propose {
        Some(signature) => {
            if !paths.is_empty() {
                return Err(EgError::Usage(
                    "--propose cannot be combined with paths".to_string(),
                ));
            }
            if signature.trim().is_empty() {
                return Err(EgError::Usage("propose must be non-empty".to_string()));
            }
            if !coverage.is_empty() {
                return Err(EgError::Usage(
                    "--coverage cannot be combined with --propose".to_string(),
                ));
            }
        }
        None if !callees.is_empty() => {
            return Err(EgError::Usage("--callees requires --propose".to_string()));
        }
        None => {}
    }
    Ok(())
}

fn execute_reuse(
    req: &CommandRequest,
    ctx: &crate::query::QueryCtx<'_>,
) -> Result<crate::query::LineQuery> {
    let CommandRequest::Reuse {
        paths,
        limit,
        deep_callee,
        min_confidence,
        propose,
        callees,
        explain,
        show_suppressed,
        coverage,
        judge,
    } = req
    else {
        unreachable!()
    };
    let min_conf = if min_confidence.is_empty() {
        crate::query::reuse::ReuseConfidence::Low
    } else {
        crate::query::reuse::ReuseConfidence::parse(min_confidence).ok_or_else(|| {
            EgError::Usage("min-confidence must be low|provisional|medium|high".to_string())
        })?
    };
    match propose {
        Some(signature) => crate::query::reuse::run_reuse_proposal(
            ctx,
            crate::query::reuse::ProposalRunArgs {
                signature: signature.clone(),
                callees: callees.clone(),
                limit: *limit,
                deep_callee: *deep_callee,
                min_confidence: min_conf,
                explain: *explain,
                show_suppressed: *show_suppressed,
                judge: *judge,
            },
        ),
        None => crate::query::run_reuse(
            ctx,
            crate::query::reuse::ReuseRunArgs {
                paths: paths.clone(),
                limit: *limit,
                deep_callee: *deep_callee,
                min_confidence: min_conf,
                explain: *explain,
                show_suppressed: *show_suppressed,
                coverage: coverage.clone(),
                judge: *judge,
            },
        ),
    }
}

/// Index-only query arms: graph traversal, analysis, and inventory.
fn run_query(req: &CommandRequest, root_arg: Option<&Path>, surface: Surface) -> Result<ArmOut> {
    if matches!(req, CommandRequest::Reuse { .. }) {
        validate_reuse_args(req)?;
    }
    let (root, conn) = open_query_for_surface(root_arg, surface)?;
    let ctx = crate::query::QueryCtx {
        conn: &conn,
        root: &root,
    };
    match req {
        CommandRequest::Callers { name, limit, depth } => emit(crate::query::callers(
            &ctx,
            name,
            graph_opts(*limit, *depth),
        )?),
        CommandRequest::Callees { name, limit, depth } => emit(crate::query::callees(
            &ctx,
            name,
            graph_opts(*limit, *depth),
        )?),
        CommandRequest::Context { name, limit } => Ok(ArmOut::from_line_query(
            crate::query::context(&ctx, name, *limit)?,
        )),
        CommandRequest::Imports { file, limit } => emit(crate::query::imports(&ctx, file, *limit)?),
        CommandRequest::Path { from, to, depth } => Ok(ArmOut::from_line_query(
            crate::query::path(&ctx, from, to, *depth)?,
        )),
        CommandRequest::Impact {
            file,
            sites,
            depth,
            edges,
            limit,
        } => emit(crate::query::impact(
            &ctx,
            file,
            *limit,
            crate::query::ImpactOpts {
                sites: *sites,
                depth: *depth,
                kinds: edges.clone(),
            },
        )?),
        CommandRequest::Unresolved { names, path, limit } => {
            let out = if *names {
                crate::query::unresolved_names(&ctx, path.as_deref(), *limit)?
            } else {
                crate::query::unresolved(&ctx, path.as_deref(), *limit)?
            };
            Ok(ArmOut::from_line_query(out))
        }
        CommandRequest::Symbols {
            file,
            outline,
            limit,
        } => Ok(ArmOut::from_line_query(crate::query::symbols(
            &ctx,
            file,
            *limit,
            crate::query::SymbolsOpts { outline: *outline },
        )?)),
        CommandRequest::Reuse { .. } => Ok(ArmOut::from_line_query(execute_reuse(req, &ctx)?)),
        CommandRequest::Search { terms, limit } => Ok(ArmOut::from_line_query(
            crate::query::search(&ctx, terms, *limit)?,
        )),
        CommandRequest::Stats { path } => {
            let line = crate::insights::stats(&ctx, path.as_deref())?;
            crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
            Ok(ArmOut {
                stdout: format!("{line}\n"),
                stderr: String::new(),
                shown: Some(1),
                exit_code: None,
            })
        }
        _ => unreachable!("run_query called with a non-query request"),
    }
}

/// Shared graph-traversal knobs for `callers`/`callees`.
fn graph_opts(limit: usize, depth: usize) -> crate::query::GraphQueryOpts {
    crate::query::GraphQueryOpts { limit, depth }
}

fn run_mem_gc(req: &CommandRequest, root_arg: Option<&Path>) -> Result<ArmOut> {
    let CommandRequest::MemGc { global } = req else {
        unreachable!("run_mem_gc called with a non-gc request")
    };
    let mut conn = open_mem_conn(root_arg, *global, crate::db::StoreMode::Mutation)?;
    let (expired, superseded, durable) = crate::db::memory_gc(&mut conn, crate::db::unix_now())?;
    crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
    Ok(ArmOut::plain(
        format!(
            "{}\nexpired={expired} removed, superseded={superseded} removed, durable={durable} rows\n",
            crate::memory::MEMORY_CAVEAT
        ),
        String::new(),
    ))
}
