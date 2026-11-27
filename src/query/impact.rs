//! Reverse-dependency impact queries and their traversal bookkeeping.

mod deep;
mod direct;

use super::*;
use deep::impact_deep;
use direct::{impact_direct_grouped, impact_direct_sites};

/// `--edges` accepted kinds plus `all`. `type_ref` rows are storage-only
/// (no surface reads them), so the CLI keeps rejecting that spelling.
const EDGE_CLI_KINDS: [EdgeKind; 3] = [EdgeKind::Calls, EdgeKind::Imports, EdgeKind::Inherits];

/// Validate a `--edges` spec into its kinds (sorted, deduplicated). Called at
/// the service boundary so `CommandRequest::Impact` carries validated kinds.
pub(crate) fn parse_edge_kinds(spec: &str) -> Result<Vec<EdgeKind>> {
    let mut out: Vec<EdgeKind> = Vec::new();
    let mut saw_all = false;
    for raw in spec.split(',') {
        let part = raw.trim();
        if part == "all" {
            saw_all = true;
            out.extend(EDGE_CLI_KINDS);
            continue;
        }
        match EdgeKind::parse(part) {
            Some(kind) if EDGE_CLI_KINDS.contains(&kind) => out.push(kind),
            _ => {
                let mut names: Vec<&str> =
                    EDGE_CLI_KINDS.iter().map(|kind| kind.as_str()).collect();
                names.push("all");
                let accepted = names.join(",");
                return Err(EgError::Usage(match part {
                    "" => format!("--edges requires {accepted}"),
                    other => format!("unknown --edges value '{other}' ({accepted})"),
                }));
            }
        }
    }
    if saw_all && spec.split(',').any(|p| p.trim() != "all") {
        return Err(EgError::Usage(
            "--edges all cannot be combined with explicit kinds".to_string(),
        ));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Source-symbol join `edges e -> symbols s`, for statements that read the
/// source symbol without its file. Same alias set as
/// [`crate::db::EDGE_SRC_FILE_JOIN`], the one home for the source-side join
/// vocabulary. Lives here because impact is its only consumer.
pub(crate) const EDGE_SRC_JOIN: &str = "JOIN symbols s ON s.id = e.src_id";

/// Destination-side join shared by impact SQL statements that filter on the
/// destination file: edge -> destination symbol. Uses denormalized `file_path`.
const IMPACT_DST_JOIN: &str = "JOIN symbols ds ON ds.id = e.dst_id";

fn impact_empty_detail(kinds: &[EdgeKind]) -> String {
    let has_imports = kinds.contains(&EdgeKind::Imports);
    let has_inherits = kinds.contains(&EdgeKind::Inherits);
    match (has_imports, has_inherits) {
        (true, true) => "0 impacting files found (unresolved imports/bases are omitted)",
        (true, false) => "0 impacting files found (unresolved imports are omitted)",
        (false, true) => "0 impacting files found (unresolved bases are omitted)",
        (false, false) => "0 impacting files found",
    }
    .to_string()
}

fn impact_no_results(ctx: &ImpactCtx<'_>) -> EgError {
    EgError::NoResultsCaveated {
        caveat: ctx.caveat,
        detail: impact_empty_detail(ctx.kinds),
    }
}

/// Impact query options: call-site vs grouped, traversal depth, and the validated
/// edge kinds.
pub struct ImpactOpts {
    pub sites: bool,
    pub depth: usize,
    pub kinds: Vec<EdgeKind>,
}

pub fn impact(
    qctx: &QueryCtx<'_>,
    file: &str,
    limit: usize,
    opts: ImpactOpts,
) -> Result<QueryOutput> {
    let ImpactOpts {
        sites,
        depth,
        kinds,
    } = opts;
    let limit = clamp_limit(limit)?;
    let depth = depth.clamp(1, crate::query::MAX_DEPTH);
    let kind_sql = db::edge_kind_sql(&kinds);
    // Symbol input: a name that is not an indexed file resolves through the
    // identity predicate to its defining file, so a symbol's blast radius is
    // queryable without knowing its path. (File-level granularity: the
    // impact of every symbol in that file is reported.)
    let norm = normalize_repo_path(qctx.root, file);
    let (rel, symbol_origin) = match indexed_file_opt(qctx.conn, &norm)? {
        Some(rel) => (rel, None),
        None => match symbol_defining_file(qctx, &norm)? {
            Some((rel, matched)) => (rel, Some(matched)),
            None => return Err(EgError::not_indexed(file)),
        },
    };
    let extended = depth > 1 || kinds.iter().any(|k| *k != EdgeKind::Calls);
    let caveat = if extended {
        if kinds.iter().any(|k| *k != EdgeKind::Calls) {
            IMPACT_EDGES_CAVEAT
        } else {
            IMPACT_CALLS_CAVEAT
        }
    } else {
        ""
    };
    if depth > 1 && sites {
        return Err(EgError::Usage(
            "--sites is only valid with --depth 1".to_string(),
        ));
    }

    let ctx = ImpactCtx {
        rel: &rel,
        limit,
        depth,
        kind_sql: &kind_sql,
        caveat,
        kinds: &kinds,
        extended,
    };
    let mut out = if depth == 1 {
        if !sites {
            impact_direct_grouped(qctx, &ctx)?
        } else {
            impact_direct_sites(qctx, &ctx)?
        }
    } else {
        impact_deep(qctx, &ctx)?
    };

    if let Some(matched) = symbol_origin {
        prepend_header(
            &mut out,
            &format!(
                "symbol resolved: '{}' -> {}",
                sanitize(file),
                sanitize(&matched)
            ),
        );
    }
    let guards =
        crate::memory::find_guarding_notes_filtered(qctx.root, &[&rel], &[], &["decision"], 1);
    for g in &guards {
        prepend_header(
            &mut out,
            &format!(
                "warning: guarded by decision '{}': {}",
                sanitize(&g.key),
                sanitize(&g.summary)
            ),
        );
    }
    Ok(out)
}

/// The file defining a symbol, routed through `resolve::target_ids`.
/// Returns `EgError::Ambiguous` when the resolved candidates span >1 distinct
/// file.
fn symbol_defining_file(qctx: &QueryCtx<'_>, name: &str) -> Result<Option<(String, String)>> {
    let (ids, cands) = target_ids(qctx.conn, qctx.root, name)?;
    if ids.is_empty() {
        return Ok(None);
    }
    let first_path = &cands[0].0;
    if cands.iter().any(|c| &c.0 != first_path) {
        return Err(EgError::Ambiguous {
            name: name.to_string(),
            total: ids.len(),
            candidates: cands,
            role: None,
        });
    }
    Ok(Some((first_path.clone(), cands[0].2.clone())))
}

/// Shared, immutable arguments for the three impact phases. The phases stay
/// contiguous; this is not a generic query framework.
struct ImpactCtx<'a> {
    rel: &'a str,
    limit: usize,
    depth: usize,
    kind_sql: &'a str,
    caveat: &'static str,
    kinds: &'a [EdgeKind],
    extended: bool,
}

fn impact_with_caveat(mut out: QueryOutput, caveat: &str, extended: bool) -> QueryOutput {
    if extended {
        prepend_header(&mut out, caveat);
    }
    out
}
