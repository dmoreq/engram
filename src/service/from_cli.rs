//! CLI-to-request conversion: command family routing and the
//! `CommandRequest::from_cli` mapping for every subcommand.

use super::*;
use crate::journal::JournalArgs;

mod memory;

/// Comma-joined `--edges` kinds for the `impact` journal line.
fn edge_kinds_csv(kinds: &[crate::extract::EdgeKind]) -> String {
    kinds
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Shared journal fields for the graph queries; `depth` joins only for the
/// two call-graph directions (callers/callees). Built in explicit branches so
/// field order never depends on a positional `insert`.
fn graph_args(name: &str, depth: Option<usize>, limit: usize) -> JournalArgs {
    match depth {
        Some(depth) => vec![
            ("name", name.to_string()),
            ("depth", depth.to_string()),
            ("limit", limit.to_string()),
        ],
        None => vec![("name", name.to_string()), ("limit", limit.to_string())],
    }
}

/// Shared trailing journal fields for the path-scoped analysis commands:
/// `head` first, then the optional path (empty when absent) and the limit.
/// Field order matches the historical journal line.
fn path_limit_args(head: &JournalArgs, path: Option<&str>, limit: usize) -> JournalArgs {
    let mut out = head.to_vec();
    out.push(("path", path.map(str::to_string).unwrap_or_default()));
    out.push(("limit", limit.to_string()));
    out
}

impl CommandRequest {
    /// CLI conversion.
    pub fn from_cli(cmd: crate::cli::Cmd) -> Result<CommandRequest> {
        Ok(match cmd {
            crate::cli::Cmd::Build {
                force,
                max_parse_error_rate,
            } => CommandRequest::Build {
                force,
                max_parse_error_rate,
            },
            crate::cli::Cmd::Doctor { invalidate } => CommandRequest::Doctor { invalidate },
            crate::cli::Cmd::Usage { quality, global } => CommandRequest::Usage { quality, global },
            crate::cli::Cmd::Audit {
                json,
                global,
                days,
                baseline_days,
                all_checks,
                reveal_paths,
                fail_on,
            } => CommandRequest::Audit {
                json,
                global,
                days,
                baseline_days,
                all_checks,
                reveal_paths,
                fail_on,
            },
            cmd @ crate::cli::Cmd::Mem { .. } => Self::from_cli_memory(cmd)?,
            crate::cli::Cmd::Mcp { .. }
            | crate::cli::Cmd::Dashboard { .. }
            | crate::cli::Cmd::Monitor { .. } => {
                unreachable!("standalone mode, handled before dispatch")
            }
            other => Self::from_cli_query(other)?,
        })
    }

    fn from_cli_query(cmd: crate::cli::Cmd) -> Result<CommandRequest> {
        Ok(match cmd {
            crate::cli::Cmd::Callers { name, limit, depth } => {
                CommandRequest::Callers { name, limit, depth }
            }
            crate::cli::Cmd::Callees { name, limit, depth } => {
                CommandRequest::Callees { name, limit, depth }
            }
            crate::cli::Cmd::Context { name, limit } => CommandRequest::Context { name, limit },
            crate::cli::Cmd::Imports { file, limit } => CommandRequest::Imports { file, limit },
            crate::cli::Cmd::Path { from, to, depth } => CommandRequest::Path { from, to, depth },
            crate::cli::Cmd::Impact {
                file,
                sites,
                depth,
                edges,
                limit,
            } => CommandRequest::Impact {
                file,
                sites,
                depth,
                edges: crate::query::parse_edge_kinds(&edges)?,
                limit,
            },
            crate::cli::Cmd::Unresolved { names, path, limit } => {
                CommandRequest::Unresolved { names, path, limit }
            }
            crate::cli::Cmd::Symbols {
                file,
                outline,
                limit,
            } => CommandRequest::Symbols {
                file,
                outline,
                limit,
            },
            crate::cli::Cmd::Reuse {
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
            } => CommandRequest::Reuse {
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
            },
            crate::cli::Cmd::Search { terms, limit } => CommandRequest::Search { terms, limit },
            crate::cli::Cmd::Stats { path } => CommandRequest::Stats { path },
            _ => unreachable!("from_cli_query called with a non-query command"),
        })
    }

    /// Tool name as journaled and keyed by the MCP tool registry.
    pub fn tool_name(&self) -> &'static str {
        match self {
            CommandRequest::Build { .. } => "build",
            CommandRequest::Callers { .. } => "callers",
            CommandRequest::Callees { .. } => "callees",
            CommandRequest::Context { .. } => "context",
            CommandRequest::Imports { .. } => "imports",
            CommandRequest::Path { .. } => "path",
            CommandRequest::Impact { .. } => "impact",
            CommandRequest::Unresolved { .. } => "unresolved",
            CommandRequest::Symbols { .. } => "symbols",
            CommandRequest::Reuse { .. } => "reuse",
            CommandRequest::Search { .. } => "search",
            CommandRequest::Stats { .. } => "stats",
            CommandRequest::Doctor { .. } => "doctor",
            CommandRequest::Usage { .. } => "usage",
            CommandRequest::Audit { .. } => "audit",
            CommandRequest::MemWrite { .. } => "mem_write",
            CommandRequest::MemTouch { .. } => "mem_touch",
            CommandRequest::MemQuery { .. } => "mem_query",
            CommandRequest::MemGet { .. } => "mem_get",
            CommandRequest::MemDelete { .. } => "mem_delete",
            CommandRequest::MemGc { .. } => "mem_gc",
            CommandRequest::MemExport { .. } => "mem_export",
            CommandRequest::MemImport { .. } => "mem_import",
            CommandRequest::MemReindex { .. } => "mem_reindex",
        }
    }

    /// Journal argument object, identical on both surfaces (one source of
    /// truth replaces the former per-surface argument tables).
    pub fn journal_args(&self) -> JournalArgs {
        match self {
            CommandRequest::Build {
                force,
                max_parse_error_rate,
            } => vec![
                ("force", force.to_string()),
                ("threshold", max_parse_error_rate.to_string()),
            ],
            CommandRequest::Doctor { invalidate } => {
                vec![("invalidate", invalidate.to_string())]
            }
            CommandRequest::Usage { quality, global } => vec![
                ("quality", quality.to_string()),
                ("global", global.to_string()),
            ],
            CommandRequest::Audit {
                days,
                baseline_days,
                global,
                ..
            } => vec![
                ("days", days.to_string()),
                ("baseline_days", baseline_days.to_string()),
                ("global", global.to_string()),
            ],
            CommandRequest::Callers { name, limit, depth }
            | CommandRequest::Callees { name, limit, depth } => {
                graph_args(name, Some(*depth), *limit)
            }
            CommandRequest::Context { name, limit } => graph_args(name, None, *limit),
            CommandRequest::Imports { file, limit } => {
                vec![("file", file.clone()), ("limit", limit.to_string())]
            }
            CommandRequest::Path { from, to, depth } => vec![
                ("name", from.clone()),
                ("to", to.clone()),
                ("depth", depth.to_string()),
            ],
            CommandRequest::Impact {
                file,
                sites,
                depth,
                edges,
                limit,
            } => vec![
                ("file", file.clone()),
                ("sites", sites.to_string()),
                ("depth", depth.to_string()),
                ("edges", edge_kinds_csv(edges)),
                ("limit", limit.to_string()),
            ],
            CommandRequest::Unresolved { names, path, limit } => {
                path_limit_args(&vec![("names", names.to_string())], path.as_deref(), *limit)
            }
            CommandRequest::Symbols {
                file,
                outline,
                limit,
            } => vec![
                ("file", file.clone()),
                ("outline", outline.to_string()),
                ("limit", limit.to_string()),
            ],
            CommandRequest::Reuse {
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
            } => vec![
                ("paths", paths.join(",")),
                ("limit", limit.to_string()),
                ("deep_callee", deep_callee.to_string()),
                ("min_confidence", min_confidence.clone()),
                ("propose", propose.clone().unwrap_or_default()),
                ("callees", callees.join(",")),
                ("explain", explain.to_string()),
                ("show_suppressed", show_suppressed.to_string()),
                ("coverage", coverage.join(",")),
                ("judge", judge.to_string()),
            ],
            CommandRequest::Search { terms, limit } => {
                vec![("terms", terms.clone()), ("limit", limit.to_string())]
            }
            CommandRequest::Stats { path } => {
                vec![("path", path.clone().unwrap_or_default())]
            }
            CommandRequest::MemWrite { .. }
            | CommandRequest::MemTouch { .. }
            | CommandRequest::MemQuery { .. }
            | CommandRequest::MemGet { .. }
            | CommandRequest::MemDelete { .. }
            | CommandRequest::MemGc { .. }
            | CommandRequest::MemReindex { .. }
            | CommandRequest::MemExport { .. }
            | CommandRequest::MemImport { .. } => self.mem_journal_args(),
        }
    }
}
